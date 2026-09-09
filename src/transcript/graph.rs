use super::models::{
    GraphEdgeKind, LineKind, TranscriptBlock, TranscriptGraph, TranscriptGraphEdge,
    TranscriptGraphNode,
};

pub fn build_transcript_graph(blocks: &[TranscriptBlock]) -> TranscriptGraph {
    let nodes = blocks
        .iter()
        .map(|block| TranscriptGraphNode {
            id: format!("node_{}", block.id),
            block_id: block.id.clone(),
            kind: block.kind.clone(),
            participant_id: block.participant_id.clone(),
        })
        .collect::<Vec<_>>();

    let mut edges = Vec::new();
    let mut current_question: Option<usize> = None;
    let mut last_interruptor: Option<usize> = None;

    for index in 0..blocks.len() {
        if index > 0 {
            edges.push(edge(index - 1, index, GraphEdgeKind::Follows, blocks));
        }

        if blocks[index].source_continuity == super::models::SourceContinuity::GapBefore {
            current_question = None;
            last_interruptor = None;
        }
        match blocks[index].kind {
            LineKind::Question => {
                if let Some(interruptor) = last_interruptor.take() {
                    edges.push(edge(
                        index,
                        interruptor,
                        GraphEdgeKind::ResumesAfter,
                        blocks,
                    ));
                }
                current_question = Some(index);
            }
            LineKind::Answer => {
                if let Some(question) = current_question {
                    edges.push(edge(index, question, GraphEdgeKind::RespondsTo, blocks));
                }
            }
            LineKind::Objection => {
                if let Some(question) = current_question {
                    edges.push(edge(index, question, GraphEdgeKind::Interrupts, blocks));
                    last_interruptor = Some(index);
                }
            }
            LineKind::InterpreterStatement => {
                if index > 0 {
                    edges.push(edge(index, index - 1, GraphEdgeKind::Interprets, blocks));
                }
            }
            LineKind::ExaminationHeading
            | LineKind::Heading
            | LineKind::RedactionMarker
            | LineKind::Parenthetical => {
                current_question = None;
                last_interruptor = None;
            }
            _ => {}
        }
    }

    TranscriptGraph { nodes, edges }
}

fn edge(
    from_index: usize,
    to_index: usize,
    kind: GraphEdgeKind,
    blocks: &[TranscriptBlock],
) -> TranscriptGraphEdge {
    TranscriptGraphEdge {
        from: format!("node_{}", blocks[from_index].id),
        to: format!("node_{}", blocks[to_index].id),
        kind,
    }
}
