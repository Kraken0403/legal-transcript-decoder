#[derive(Debug, Clone)]
pub struct TranscriptProfile {
    pub name: &'static str,
    pub max_line_number: u16,
    pub word_question_markers: &'static [&'static str],
    pub word_answer_markers: &'static [&'static str],
}

impl TranscriptProfile {
    pub fn us_english() -> Self {
        Self {
            name: "us_english",
            max_line_number: 100,
            word_question_markers: &["QUESTION:", "QUESTION."],
            word_answer_markers: &["ANSWER:", "ANSWER."],
        }
    }
}
