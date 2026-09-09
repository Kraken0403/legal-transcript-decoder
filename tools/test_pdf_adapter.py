"""Non-build extraction regressions. Run: python -m unittest discover -s tools -p test_pdf_adapter.py
Test-only dependency: reportlab. Runtime dependencies are in requirements-parser.txt.
"""
import importlib.util
import io
import json
import shutil
import unittest
from pathlib import Path
from reportlab.pdfgen import canvas
from reportlab.lib.utils import ImageReader
SPEC=importlib.util.spec_from_file_location('pdf_adapter',Path(__file__).resolve().parents[1]/'src/document/pdf_adapter.py')
ADAPTER=importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ADAPTER)

def pdf(draw):
    buf=io.BytesIO();c=canvas.Canvas(buf,pagesize=(612,792));draw(c);c.save();return buf.getvalue()

def pane(c,x,y,page,tag,right=False):
    c.setFont('Helvetica',9);c.drawString(x+185,y+20,str(page))
    for i in range(1,9):
        c.drawString(x+(215 if right else 0),y-i*18,str(i))
        c.drawString(x+(0 if right else 28),y-i*18,('Q. ' if i%2 else 'A. ')+tag+str(i))

class CoordinateTests(unittest.TestCase):
    def test_proportional_spacing_numbers_and_hyphens(self):
        def draw(c):
            c.setFont('Helvetica',12);c.drawString(540,767,'7')
            for i,text in enumerate(['Q. Did 12 jurors see it?','A. Yes, on 9/11.','MR. SMITH: A product failed.','Q. Spell it.','A. B-o-n-d-i.','Q. Was it sex-trafficking?'],1):
                c.drawRightString(55,730-i*24,str(i));c.drawString(90,730-i*24,text)
        p=ADAPTER.extract(pdf(draw))['pages'][0]
        self.assertIn('B-o-n-d-i.',p['text']);self.assertIn('sex-trafficking',p['text']);self.assertIn('12 jurors',p['text'])
        self.assertEqual(p['layout']['numbered_row_count'],6);self.assertEqual(p['visual_rows'][0]['printed_page'],'7')
    def test_right_side_gutter(self):
        p=ADAPTER.extract(pdf(lambda c:pane(c,65,700,8,'RIGHT',True)))['pages'][0]
        self.assertEqual([r['line_number'] for r in p['visual_rows'] if r['line_number']],list(range(1,9)))
        self.assertIn('1 Q. RIGHT1',p['text'])
    def test_two_panes_not_interleaved(self):
        def draw(c):pane(c,30,700,10,'LEFT');pane(c,330,700,11,'RIGHT')
        p=ADAPTER.extract(pdf(draw))['pages'][0]
        self.assertEqual(p['layout']['column_count'],2);self.assertLess(p['text'].index('LEFT8'),p['text'].index('RIGHT1'))
        self.assertEqual({r['printed_page'] for r in p['visual_rows']},{'10','11'})
    def test_four_panes_read_top_then_bottom(self):
        def draw(c):
            pane(c,30,700,10,'TL');pane(c,330,700,11,'TR');pane(c,30,350,12,'BL');pane(c,330,350,13,'BR')
        p=ADAPTER.extract(pdf(draw))['pages'][0]
        self.assertEqual(p['layout']['column_count'],4)
        positions=[p['text'].index(v) for v in ['TL8','TR1','TR8','BL1','BL8','BR1']]
        self.assertEqual(positions,sorted(positions))
    def test_black_mask_hides_underlying_text(self):
        def draw(c):
            c.setFont('Helvetica',12);c.drawString(90,710,'MR. SECRET: Visible answer.');c.setFillColorRGB(0,0,0);c.rect(88,706,79,17,fill=1,stroke=0)
        p=ADAPTER.extract(pdf(draw))['pages'][0]
        self.assertNotIn('SECRET',json.dumps(p));self.assertIn('[REDACTED]',p['text']);self.assertIn('Visible answer.',p['text'])
    def test_blank_page_preserved(self):
        def draw(c):c.showPage();c.drawString(90,700,'Q. Was the first page blank?')
        pages=ADAPTER.extract(pdf(draw))['pages'];self.assertEqual(len(pages),2);self.assertEqual(pages[0]['text'],'')
    @unittest.skipUnless(shutil.which('tesseract'),'Tesseract not installed')
    def test_image_only_page_uses_local_ocr(self):
        import pypdfium2
        original=pdf(lambda c:pane(c,55,700,1,'TEST'))
        with pypdfium2.PdfDocument(original) as doc:
            p=doc[0];bitmap=p.render(scale=3);image=bitmap.to_pil().copy();bitmap.close();p.close()
        scanned=pdf(lambda c:c.drawImage(ImageReader(image),0,0,width=612,height=792));image.close()
        p=ADAPTER.extract(scanned)['pages'][0]
        self.assertTrue(p['layout']['ocr_performed']);self.assertFalse(p['layout']['requires_ocr'])
        self.assertIn('TEST',p['text']);self.assertIsNotNone(p['layout']['ocr_mean_confidence'])
    def test_sparse_page_inherits_only_geometric_gutter(self):
        def draw(c):
            pane(c,55,700,1,'FIRST');c.showPage();c.drawString(540,750,'2');c.drawString(55,682,'1');c.drawString(83,682,'Q. Only one printed row?')
        pages=ADAPTER.extract(pdf(draw))['pages'];self.assertEqual(pages[1]['layout']['numbered_row_count'],1)

    def test_nested_form_text_is_extracted(self):
        def draw(c):
            c.beginForm('inner');c.setFont('Helvetica',12);c.drawString(0,0,'Q. Nested form question?');c.endForm()
            c.beginForm('outer');c.saveState();c.translate(20,0);c.doForm('inner');c.restoreState();c.endForm()
            c.saveState();c.translate(80,700);c.doForm('outer');c.restoreState()
            c.drawString(100,670,'A. Nested form answer.')
        p=ADAPTER.extract(pdf(draw))['pages'][0]
        self.assertIn('Q. Nested form question?',p['text']);self.assertIn('A. Nested form answer.',p['text'])
    def test_unnumbered_speaker_columns_are_separated(self):
        def draw(c):
            c.setFont('Helvetica',9)
            for i in range(6):
                c.drawString(40,700-i*25,'ALICE: LEFT'+str(i))
                c.drawString(340,700-i*25,'BOB: RIGHT'+str(i))
        p=ADAPTER.extract(pdf(draw))['pages'][0]
        self.assertEqual(p['layout']['column_count'],2)
        self.assertLess(p['text'].index('LEFT5'),p['text'].index('RIGHT0'))
        self.assertTrue(p['layout']['reading_order_uncertain'])
    def test_right_gutters_in_two_panes(self):
        def draw(c):pane(c,30,700,10,'LEFT',True);pane(c,330,700,11,'RIGHT',True)
        p=ADAPTER.extract(pdf(draw))['pages'][0]
        self.assertEqual(p['layout']['column_count'],2)
        self.assertLess(p['text'].index('LEFT8'),p['text'].index('RIGHT1'))

if __name__=='__main__':unittest.main()
