"""Local coordinate extraction. stdin=PDF bytes; stdout=versioned JSON.
No network calls. Requires pdfplumber + pypdfium2 + Pillow; Tesseract is optional.
Coordinates in rows/fragments are points in displayed page space, origin top left.
"""
import csv
import io
import json
import math
import os
import re
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path

MAX_PAGES = 3000
MAX_CHARS_PER_PAGE = 250000
MAX_INPUT = 50 * 1024 * 1024


def median(values, default=10):
    return statistics.median(values) if values else default


def dark(color):
    if color is None:
        return False
    values = [color] if isinstance(color, (float, int)) else list(color)
    if len(values) == 4:
        return values[3] >= .85
    return bool(values) and max(values) <= .12


def box(obj):
    return [float(obj['x0']), float(obj['top']), float(obj['x1']), float(obj['bottom'])]


def overlap(a, b):
    area = max(0, min(a[2], b[2]) - max(a[0], b[0])) * max(0, min(a[3], b[3]) - max(a[1], b[1]))
    return area / max(.01, (a[2]-a[0]) * (a[3]-a[1]))


def union_boxes(items):
    return [min(x['x0'] for x in items), min(x['top'] for x in items),
            max(x['x1'] for x in items), max(x['bottom'] for x in items)]


def char_words(chars):
    """Use actual glyph advances, never character-count width estimates."""
    heights = [c['bottom'] - c['top'] for c in chars if c.get('text', '').strip()]
    tolerance = max(.8, min(3, median(heights) * .24))
    groups = []
    for c in sorted(chars, key=lambda c: (c['bottom'], c['x0'])):
        if not c.get('text') or not all(math.isfinite(c[k]) for k in ['x0', 'x1', 'top', 'bottom']):
            continue
        candidates = [(abs(c['bottom']-g[0]), i) for i,g in enumerate(groups[-5:]) if abs(c['bottom']-g[0]) <= tolerance]
        if candidates:
            _, relative = min(candidates)
            groups[len(groups)-min(5,len(groups))+relative][1].append(c)
        else:
            groups.append([c['bottom'], [c]])
    words = []
    for _, group in groups:
        current = []
        def flush():
            if not current:
                return
            b = union_boxes(current)
            words.append(dict(text=''.join(c['text'] for c in current), x0=b[0], top=b[1], x1=b[2], bottom=b[3],
                              size=median([c.get('size', c['bottom']-c['top']) for c in current]),
                              font=current[0].get('fontname'), source='native_pdf', confidence=None,
                              masked=any(c.get('masked') for c in current)))
            current.clear()
        for c in sorted(group, key=lambda c: c['x0']):
            if c['text'].isspace():
                flush()
                continue
            if current and c['x0'] - current[-1]['x1'] > max(.7, c.get('size',10)*.22):
                flush()
            # Overprinted copies may be duplicate OCR/native layers. Only exact same-position glyphs collapse.
            if current and c['text'] == current[-1]['text'] and abs(c['x0']-current[-1]['x0']) < .15 and abs(c['top']-current[-1]['top']) < .15:
                continue
            current.append(c)
        flush()
    return words


def group_rows(words):
    groups = []
    tolerance = max(1, min(3.5, median([w.get('size',10) for w in words])*.3))
    for w in sorted(words, key=lambda w: ((w['top']+w['bottom'])/2, w['x0'])):
        y = (w['top']+w['bottom'])/2
        if groups and abs(y-groups[-1][0]) <= tolerance:
            groups[-1][1].append(w)
        else:
            groups.append([y,[w]])
    return [sorted(g[1],key=lambda w:w['x0']) for g in groups]


def gutter_columns(words):
    numbers = [w for w in words if re.fullmatch(r'\d{1,3}',w['text']) and 1<=int(w['text'])<=100]
    columns=[]
    for w in sorted(numbers,key=lambda w:w['x1']):
        if columns and abs(w['x1']-median([x['x1'] for x in columns[-1]]))<=7:
            columns[-1].append(w)
        else:
            columns.append([w])
    good=[]
    for col in columns:
        col.sort(key=lambda w:w['top'])
        nums=[int(w['text']) for w in col]
        transitions=sum(0<b-a<=3 or (b<=3 and a>=8) for a,b in zip(nums,nums[1:]))
        if len(set(nums))>=4 and transitions/max(1,len(nums)-1)>=.65 and max(nums)-min(nums)>=3:
            good.append(col)
    return good


def assemble_rows(words,width,height):
    """Recognize independent numbered panes before doing horizontal row joins."""
    gutters=gutter_columns(words)
    # Gutter candidates are further required to sit beside a persistent empty gap.
    reliable=[]
    for col in gutters:
        x=median([w['x0'] for w in col]); xr=median([w['x1'] for w in col])
        near_left=sum(0<x-w['x1']<18 for w in words if not w['text'].isdigit())
        near_right=sum(0<w['x0']-xr<18 for w in words if not w['text'].isdigit())
        if min(near_left,near_right) <= max(2,len(col)*.3):
            reliable.append(col)
    gutters=reliable
    gutters.sort(key=lambda c:median([w['x0'] for w in c]))
    # Adjacent numerical columns do not constitute separate transcript panes.
    gutters=[c for i,c in enumerate(gutters) if i==0 or median([w['x0'] for w in c])-median([w['x0'] for w in gutters[i-1]])>width*.20]
    cuts=[0.0]
    for left,right in zip(gutters,gutters[1:]):
        lx=median([w['x1'] for w in left]); rx=median([w['x0'] for w in right])
        span=rx-lx
        body=[w for w in words if not w['text'].isdigit()]
        to_left=sum(lx-span<w['x1']<lx for w in body)
        to_right=sum(lx<w['x0']<rx for w in body)
        first_gutter=median([w['x0'] for w in gutters[0]])
        leftmost_body=min((w['x0'] for w in body),default=first_gutter)
        right_gutters=leftmost_body < first_gutter-12
        preferred=lx+16 if right_gutters else rx-16
        candidates=[(sum(w['x0']<v<w['x1'] for w in words),abs(v-preferred),v) for v in [rx-16,lx+16,(rx+lx)/2]]
        cuts.append(min(candidates)[2])
    uncertain=False
    if not gutters:
        gaps=[]
        for row in group_rows(words):
            for j,(a,b) in enumerate(zip(row,row[1:])):
                if b['x0']-a['x1']<width*.08:
                    continue
                left=' '.join(w['text'] for w in row[:j+1])
                right=' '.join(w['text'] for w in row[j+1:])
                anchor=r'^(?:[A-Z][A-Z .\'-]{1,50}:|(?:Mr|Ms|Dr)\. [A-Z][\w-]+\.|[QA][.:])'
                if re.match(anchor,left) and re.match(anchor,right):
                    gaps.append((a['x1'],b['x0']))
        if len(gaps)>=4:
            cut=median([b for a,b in gaps])-12
            if not any(w['x0']<cut<w['x1'] for w in words):
                cuts.append(cut);uncertain=True
    cuts.append(width+1)
    panes=[]
    for ci,(x0,x1) in enumerate(zip(cuts,cuts[1:])):
        chunk=[w for w in words if x0<=(w['x0']+w['x1'])/2<x1]
        col=gutters[ci] if ci<len(gutters) else []
        ycuts=[0.0]
        for a,b in zip(col,col[1:]):
            if int(b['text'])<=3 and int(a['text'])>=8 and b['top']-a['bottom']>8:
                ycuts.append((a['bottom']+b['top'])/2)
        ycuts.append(height+1)
        for y0,y1 in zip(ycuts,ycuts[1:]):
            pane=[w for w in chunk if y0<=(w['top']+w['bottom'])/2<y1]
            if pane:
                panes.append((y0,x0,pane,col))
    # Standard 4-up reading order is top-left, top-right, bottom-left, bottom-right.
    panes.sort(key=lambda p:(round(p[0]/20),p[1]))
    output=[]
    for pi,(_,_,pane,col) in enumerate(panes):
        gutter_ids={id(w) for w in col}
        grouped=group_rows(pane)
        numbered=[(j, r) for j,r in enumerate(grouped) if any(id(w) in gutter_ids for w in r)]
        page_label=None
        first_body=numbered[0][0] if numbered else 1
        for j,r in enumerate(grouped):
            if len(r)==1 and re.fullmatch(r'\d{1,7}',r[0]['text']) and id(r[0]) not in gutter_ids:
                if j<first_body or j==len(grouped)-1:
                    page_label=str(int(r[0]['text']))
                    break
            joined=' '.join(w['text'] for w in r)
            m=re.fullmatch(r'(?:Page|PAGE)\s+(\d+)(?:\s+of\s+\d+)?',joined)
            if m and (j<3 or j>=len(grouped)-3):
                page_label=m.group(1)
        for ri,r in enumerate(grouped):
            gutter=[w for w in r if id(w) in gutter_ids]
            if len(gutter)>1:
                uncertain=True
            number=int(gutter[0]['text']) if len(gutter)==1 else None
            body=[w for w in r if not gutter or w is not gutter[0]]
            fragments=[]
            for w in body:
                fragments.append(dict(text=w['text'],x=w['x0'],y=-w['bottom'],width=w['x1']-w['x0'],height=w['bottom']-w['top'],font_name=w.get('font'),font_size=w.get('size'),source=w.get('source','native_pdf'),sequence=len(fragments),geometry_estimated=False,redaction_masked=w.get('masked',False)))
            content=' '.join(w['text'] for w in body)
            text=(str(number)+(' '+content if content else '')) if number is not None else content
            conf=[w['confidence'] for w in body if w.get('confidence') is not None]
            output.append(dict(y=-median([w['bottom'] for w in r]),left_x=body[0]['x0'] if body else None,
                               line_number_x=gutter[0]['x0'] if len(gutter)==1 else None,line_number=number,text=text,fragments=fragments,
                               panel=pi,printed_page=page_label,bbox=union_boxes(r),ocr_confidence=sum(conf)/len(conf) if conf else None))
    return output,len(panes),uncertain


def ocr_words(pdfium_page,width,height):
    import pypdfium2 as pdfium
    scale=min(300/72,math.sqrt(25_000_000/max(1,width*height)))
    bitmap=pdfium_page.render(scale=scale)
    image=bitmap.to_pil()
    with tempfile.TemporaryDirectory(prefix='transcript-ocr-') as tmp:
        path=Path(tmp)/'page.png'
        image.save(path)
        proc=subprocess.run([os.environ.get('TRANSCRIPT_TESSERACT','tesseract'),str(path),'stdout','-l',os.environ.get('TRANSCRIPT_OCR_LANGUAGE','eng'),'--psm','3','tsv'],capture_output=True,timeout=90,check=False)
    image.close();bitmap.close()
    if proc.returncode:
        raise RuntimeError('Tesseract could not OCR this page. Check installation and language data.')
    words=[]
    for r in csv.DictReader(io.StringIO(proc.stdout.decode('utf-8','replace')),delimiter='\t'):
        if r.get('level')!='5' or not r.get('text','').strip():
            continue
        x,y,w,h=[float(r[k])/scale for k in ['left','top','width','height']]
        confidence=max(0,min(1,float(r['conf'])/100))
        words.append(dict(text=r['text'],x0=x,top=y,x1=x+w,bottom=y+h,size=h,source='ocr',confidence=confidence,font=None,masked=False))
    return words


def extract(data):
    import pdfplumber
    import pypdfium2 as pdfium
    pages=[]
    with pdfplumber.open(io.BytesIO(data),unicode_norm=None) as pdf, pdfium.PdfDocument(data) as rendered:
        if len(pdf.pages)>MAX_PAGES:
            raise ValueError('PDF exceeds 3000-page extraction limit; split into volumes.')
        for index,page in enumerate(pdf.pages):
            warnings=[]
            chars=page.chars
            if len(chars)>MAX_CHARS_PER_PAGE:
                raise ValueError('PDF page exceeds glyph limit; no partial result was emitted.')
            masks=[];annotations=[];rectangles=[]
            for rect in page.rects:
                possible=rect.get('fill') and dark(rect.get('non_stroking_color')) and rect['height']>=3 and rect['width']>=8 and rect['height']<=page.height*.2
                if possible:
                    masks.append(box(rect))
                    rectangles.append(dict(physical_page=index+1,rect=[rect['x0'],rect['y0'],rect['width'],rect['height']],color_space='gray',color_components=[0.0],is_dark=True,possible_redaction=True))
            for annot in page.annots or []:
                raw=annot.get('data',{})
                subtype=str(raw.get('Subtype','')).strip("/'")
                if 'Redact' in subtype:
                    masks.append(box(annot))
                    annotations.append(dict(physical_page=index+1,subtype='Redact',rect=[annot['x0'],page.height-annot['bottom'],annot['x1'],page.height-annot['top']],color_components=[],is_explicit_redaction=True,is_dark_overlay_candidate=False))
            clean=[];masked=0
            for c in chars:
                if any(overlap(box(c),m)>.25 for m in masks):
                    masked+=1
                else:
                    clean.append(c)
            words=char_words(clean)
            for m in masks:
                words.append(dict(text='[REDACTED]',x0=m[0],top=m[1],x1=m[2],bottom=m[3],size=m[3]-m[1],source='native_pdf',confidence=None,font=None,masked=True))
            if masks:
                warnings.append('Visible dark overlays/redaction annotations retained as [REDACTED]; inspect each candidate against the page.')
            bad=sum('\ufffd' in c.get('text','') or '(cid:' in c.get('text','') for c in chars)
            image_area=sum(max(0,i['width'])*max(0,i['height']) for i in page.images)
            needs_ocr=(len(''.join(c.get('text','') for c in clean).strip())<30 and (page.images or page.curves)) or bad>max(5,len(chars)*.03)
            # Image-heavy pages with a small native header still need full-page OCR.
            needs_ocr=needs_ocr or (image_area>page.width*page.height*.35 and len(clean)<500) or sum(not c.get('upright',True) for c in chars)>max(5,len(chars)*.25)
            used_ocr=False
            if needs_ocr:
                try:
                    rp=rendered[index]
                    try: candidate=ocr_words(rp,page.width,page.height)
                    finally: rp.close()
                    if not candidate:
                        raise RuntimeError('OCR returned no readable text.')
                    words=[w for w in candidate if not any(overlap(box(w),m)>.25 for m in masks)]
                    for m in masks:
                        words.append(dict(text='[REDACTED]',x0=m[0],top=m[1],x1=m[2],bottom=m[3],size=m[3]-m[1],source='ocr',confidence=None,font=None,masked=True))
                    used_ocr=True;needs_ocr=False
                    warnings.append('OCR transcription requires visual review; confidence is an engine score, not proof of correctness.')
                except (OSError,RuntimeError,subprocess.TimeoutExpired) as exc:
                    warnings.append(str(exc))
            rows,columns,uncertain=assemble_rows(words,page.width,page.height)
            if columns>1:
                warnings.append('Multiple transcript panes detected; verify printed page order before analysis.')
            if any(not c.get('upright',True) for c in chars):
                uncertain=True;warnings.append('Rotated text detected; verify reading order.')
            if image_area>page.width*page.height*.25 and not used_ocr and not needs_ocr:
                warnings.append('Substantial image content accompanies native text; image-only content may require review.')
            text='\n'.join(r['text'] for r in rows)
            ocr_scores=[r['ocr_confidence'] for r in rows if r['ocr_confidence'] is not None]
            pages.append(dict(physical_page=index+1,text=text,raw_text=text,section='unknown',section_confidence=0,section_reasons=[],visual_rows=rows,
                              layout=dict(extraction_engine='pdfplumber_pdfium_tesseract',positioned_text_available=not used_ocr and bool(chars),used_positioned_reconstruction=True,
                                          fragment_count=sum(len(r['fragments']) for r in rows),visual_row_count=len(rows),numbered_row_count=sum(r['line_number'] is not None for r in rows),
                                          masked_redaction_fragments=masked,likely_line_number_column_x=next((r['line_number_x'] for r in rows if r['line_number_x'] is not None),None),
                                          reconstruction_confidence=.80 if used_ocr else (.65 if uncertain else .95),fallback_reason=None,requires_ocr=bool(needs_ocr),warnings=warnings,
                                          ocr_performed=used_ocr,ocr_mean_confidence=sum(ocr_scores)/len(ocr_scores) if ocr_scores else None,column_count=columns,reading_order_uncertain=uncertain),
                              annotations=annotations,filled_rectangles=rectangles,image_count=len(page.images)))
            page.close()
    # Sparse continuation pages may contain only one or two printed lines. Recover their
    # gutter from the same position on adjacent, well-evidenced pages, never from a body number alone.
    for i,page in enumerate(pages):
        if page['layout']['numbered_row_count'] or page['layout']['column_count']>1:
            continue
        neighbors=pages[max(0,i-2):i]+pages[i+1:i+3]
        xs=[p['layout']['likely_line_number_column_x'] for p in neighbors if p['layout']['numbered_row_count']>=8 and p['layout']['column_count']==1]
        if not xs:
            continue
        x=median(xs)
        for row in page['visual_rows']:
            fs=row['fragments']
            if len(fs)>=2 and fs[0]['text'].isdigit() and 1<=int(fs[0]['text'])<=100 and abs(fs[0]['x']-x)<=8:
                row['line_number']=int(fs[0]['text']);row['line_number_x']=fs[0]['x'];row['fragments']=fs[1:]
                page['layout']['numbered_row_count']+=1
        if page['layout']['numbered_row_count']:
            page['layout']['likely_line_number_column_x']=x
    return dict(protocol_version=1,pages=pages)


def main():
    data=sys.stdin.buffer.read(MAX_INPUT+1)
    if len(data)>MAX_INPUT or not data.startswith(b'%PDF-'):
        raise ValueError('Expected a PDF of at most 50 MiB.')
    result=extract(data)
    json.dump(result,sys.stdout,ensure_ascii=True,separators=(',',':'),allow_nan=False)


if __name__=='__main__':
    try:
        main()
    except Exception as error:
        print('Coordinate extraction failed: '+str(error),file=sys.stderr)
        sys.exit(2)
