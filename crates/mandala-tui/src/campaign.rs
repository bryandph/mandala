//! Campaign screen state and rendering. Requests carry the exact reviewed
//! generation/names; runtime effects live in App and use the shared facade.

use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Borders, Paragraph},
};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum Review {
    Create,
    Call { tool: String, args: Value },
}
#[derive(Debug, Clone)]
pub struct Prompt {
    pub close: bool,
    pub generation: u64,
    pub id: String,
    pub text: String,
}
#[derive(Debug, Clone)]
pub struct CampaignView {
    pub request: u64,
    pub payload: Value,
    pub id: Option<String>,
    pub cursor: usize,
    pub scroll: usize,
    pub offset: usize,
    pub busy: bool,
    pub status: String,
    pub review: Option<Review>,
    pub prompt: Option<Prompt>,
    pub boot: bool,
    pub halt: bool,
    pub cache_push: Option<bool>,
    pub serial: String,
    pub drain: bool,
}
impl CampaignView {
    pub fn new() -> Self {
        Self {
            request: 0,
            payload: Value::Null,
            id: None,
            cursor: 0,
            scroll: 0,
            offset: 0,
            busy: false,
            status: "Loading campaigns".into(),
            review: None,
            prompt: None,
            boot: false,
            halt: false,
            cache_push: None,
            serial: "1".into(),
            drain: true,
        }
    }
    pub fn selected_id(&self) -> Option<&str> {
        self.id.as_deref().or_else(|| {
            self.payload["campaigns"]
                .as_array()
                .and_then(|rows| rows.get(self.cursor))
                .and_then(|row| row["campaign_id"].as_str())
        })
    }
    pub fn selected_attempt(&self) -> Option<(&str, &str)> {
        self.payload["attempts"]
            .as_array()
            .and_then(|rows| rows.get(self.cursor))
            .and_then(|row| Some((row["kind"].as_str()?, row["run_id"].as_str()?)))
            .or_else(|| {
                self.payload["run_id"]
                    .as_str()
                    .map(|id| (self.payload["run_kind"].as_str().unwrap_or("deploy"), id))
            })
    }
    pub fn cursor_len(&self) -> usize {
        self.payload[if self.id.is_some() {
            "attempts"
        } else {
            "campaigns"
        }]
        .as_array()
        .map_or(0, Vec::len)
    }
}
impl Default for CampaignView {
    fn default() -> Self {
        Self::new()
    }
}

pub fn render(frame: &mut Frame<'_>, area: Rect, view: &CampaignView) {
    let mut text = format!(
        "{}{}\n",
        if view.busy { "Working: " } else { "" },
        view.status
    );
    text.push_str(&format!(
        "Selected: {} | attempt/list row {} | page offset {}\n",
        view.selected_id().unwrap_or("none"),
        view.cursor + 1,
        view.offset
    ));
    if let Some(prompt) = &view.prompt {
        text.push_str(if prompt.close {
            "Close reason (required): "
        } else {
            "Exclusion: +node-a,node-b reason  OR  -node-b reason: "
        });
        text.push_str(&prompt.text);
        text.push_str("\nEnter reviews; Esc cancels.\n");
    } else if let Some(review) = &view.review {
        text.push_str(match review{Review::Create=>"Review frozen target below: y create-and-deploy | p bind preview only | Esc cancel\n",Review::Call{..}=>"Review exact action below: y confirm | Esc cancel (no generation lock held during review)\n"});
        if let Review::Call { tool, args } = review {
            text.push_str(&format!(
                "{tool}: {}\n",
                serde_json::to_string_pretty(args).unwrap_or_default()
            ));
        }
    } else {
        text.push_str("Enter inspect | r refresh/source check | c capture new target | t real retry | T dry retry\n");
        text.push_str("b reboot review | v verify | x exclusion | z close | d dual drift | E evaluate latest | a attach attempt\n");
        text.push_str(&format!(
            "B new boot={} | h halt={} | P cache={:?} | reboot order 1/2/3={} g drain={}\n",
            view.boot, view.halt, view.cache_push, view.serial, view.drain
        ));
        text.push_str("arrows select | PgUp/PgDn scroll | [/] membership pages | Esc back\n");
    }
    text.push('\n');
    text.push_str(&serde_json::to_string_pretty(&view.payload).unwrap_or_default());
    frame.render_widget(
        Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Deployment campaigns (explicit opt-in)"),
            )
            .scroll((view.scroll.min(u16::MAX as usize) as u16, 0)),
        area,
    );
}
