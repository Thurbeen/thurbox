use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::harness::Authority;
use crate::meta::Millis;
use crate::session::AgentState;
use crate::signal::Reported;

mod bundled;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Screen {
    pub text: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenState {
    Working,
    Idle,
    Blocked,
}

impl ScreenState {
    pub fn agent_state(self) -> AgentState {
        match self {
            ScreenState::Working => AgentState::Working,
            ScreenState::Idle => AgentState::Idle,
            ScreenState::Blocked => AgentState::Attention,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    BottomLines(usize),
    Whole,
    OscTitle,
}

impl TryFrom<String> for Region {
    type Error = String;

    fn try_from(s: String) -> Result<Region, String> {
        match s.as_str() {
            "whole" => return Ok(Region::Whole),
            "osc_title" => return Ok(Region::OscTitle),
            _ => {}
        }
        let n = s
            .strip_prefix("bottom_lines(")
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(|n| n.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                format!("region {s:?}: use bottom_lines(<n> > 0), whole or osc_title")
            })?;
        Ok(Region::BottomLines(n))
    }
}

impl Region {
    fn read(self, screen: &Screen) -> String {
        match self {
            Region::Whole => screen.text.clone(),
            Region::OscTitle => screen.title.clone(),
            Region::BottomLines(n) => {
                let lines: Vec<&str> = screen.text.lines().collect();
                let end = lines
                    .iter()
                    .rposition(|l| !l.trim().is_empty())
                    .map_or(0, |i| i + 1);
                lines[end.saturating_sub(n)..end].join("\n")
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Matcher {
    #[serde(default)]
    pub contains: Vec<String>,
    #[serde(default)]
    pub all: Vec<Matcher>,
    #[serde(default)]
    pub any: Vec<Matcher>,
    #[serde(default)]
    pub not: Option<Box<Matcher>>,
}

impl Matcher {
    fn is_empty(&self) -> bool {
        self.contains.is_empty() && self.all.is_empty() && self.any.is_empty() && self.not.is_none()
    }

    fn check(&self) -> Result<(), String> {
        if self.is_empty() {
            return Err("a matcher needs contains, all, any or not".into());
        }
        if self.contains.iter().any(String::is_empty) {
            return Err("contains holds an empty string, which every screen holds".into());
        }
        for m in self.all.iter().chain(&self.any).chain(self.not.as_deref()) {
            m.check()?;
        }
        Ok(())
    }

    fn matches(&self, text: &str) -> bool {
        self.contains.iter().all(|s| text.contains(s.as_str()))
            && self.all.iter().all(|m| m.matches(text))
            && (self.any.is_empty() || self.any.iter().any(|m| m.matches(text)))
            && self.not.as_ref().is_none_or(|m| !m.matches(text))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: String,
    pub state: ScreenState,
    pub priority: u32,
    pub region: Region,
    pub matcher: Matcher,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleText {
    id: String,
    state: ScreenState,
    priority: u32,
    region: String,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    all: Vec<Matcher>,
    #[serde(default)]
    any: Vec<Matcher>,
    #[serde(default)]
    not: Option<Box<Matcher>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesText {
    #[serde(default)]
    rule: Vec<RuleText>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    pub name: String,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub state: ScreenState,
    pub rule: String,
}

impl Rules {
    pub fn parse(name: &str, text: &str) -> Result<Rules, String> {
        let parsed: RulesText = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut rules: Vec<Rule> = Vec::new();
        for r in parsed.rule {
            if r.id.trim().is_empty() {
                return Err("a rule needs an id".into());
            }
            if rules.iter().any(|q| q.id == r.id) {
                return Err(format!("rule {:?} is listed twice", r.id));
            }
            let matcher = Matcher {
                contains: r.contains,
                all: r.all,
                any: r.any,
                not: r.not,
            };
            matcher
                .check()
                .map_err(|e| format!("rule {:?}: {e}", r.id))?;
            rules.push(Rule {
                region: Region::try_from(r.region).map_err(|e| format!("rule {:?}: {e}", r.id))?,
                id: r.id,
                state: r.state,
                priority: r.priority,
                matcher,
            });
        }
        Ok(Rules {
            name: name.to_string(),
            rules,
        })
    }

    pub fn evaluate(&self, screen: &Screen) -> Option<Seen> {
        let mut best: Option<&Rule> = None;
        for r in &self.rules {
            if best.is_some_and(|b| b.priority >= r.priority) {
                continue;
            }
            if r.matcher.matches(&r.region.read(screen)) {
                best = Some(r);
            }
        }
        best.map(|r| Seen {
            state: r.state,
            rule: format!("{}/{}", self.name, r.id),
        })
    }
}

pub fn bundled() -> &'static [Rules] {
    static ALL: OnceLock<Vec<Rules>> = OnceLock::new();
    ALL.get_or_init(|| {
        bundled::ALL
            .iter()
            .map(|(name, text)| Rules::parse(name, text).expect("a bundled rule set parses"))
            .collect()
    })
}

pub fn rules(name: &str) -> Option<&'static Rules> {
    bundled().iter().find(|r| r.name == name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Hook,
    Screen,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub state: AgentState,
    pub source: Source,
    pub rule: Option<String>,
}

pub fn merge(authority: Authority, reported: &Reported, seen: Option<&Seen>) -> Merged {
    let hook = reported.reported().then(|| Merged {
        state: AgentState::reported(reported),
        source: Source::Hook,
        rule: None,
    });
    let screen = seen.map(|s| Merged {
        state: s.state.agent_state(),
        source: Source::Screen,
        rule: Some(s.rule.clone()),
    });
    let first = match authority {
        Authority::Full => hook.or(screen),
        Authority::Partial | Authority::Session | Authority::None => screen.or(hook),
    };
    first.unwrap_or(Merged {
        state: AgentState::Unknown,
        source: Source::None,
        rule: None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentActivity {
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    pub at: Millis,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::State;

    fn screen(text: &str, title: &str) -> Screen {
        Screen {
            text: text.into(),
            title: title.into(),
        }
    }

    const SET: &str = r#"
[[rule]]
id = "ask"
state = "blocked"
priority = 900
region = "bottom_lines(2)"
contains = ["Allow?", "esc"]

[[rule]]
id = "spinner"
state = "working"
priority = 500
region = "osc_title"
any = [{ contains = ["⠋"] }, { contains = ["⠙"] }]

[[rule]]
id = "composer"
state = "idle"
priority = 100
region = "whole"
contains = ["› "]
not = { contains = ["Allow?"] }
"#;

    #[test]
    fn the_highest_priority_match_wins_on_its_region() {
        let rules = Rules::parse("x", SET).unwrap();
        let seen = |text: &str, title: &str| rules.evaluate(&screen(text, title));
        assert_eq!(
            seen("› \nAllow?\nesc to cancel\n\n\n", "⠋ x"),
            Some(Seen {
                state: ScreenState::Blocked,
                rule: "x/ask".into()
            })
        );
        assert_eq!(
            seen("Allow?\nesc\nmore\nlines\n", "⠙ x").map(|s| s.rule),
            Some("x/spinner".into())
        );
        assert_eq!(seen("› \n", "x").map(|s| s.state), Some(ScreenState::Idle));
        assert_eq!(seen("Allow?\n› \n", "x"), None, "not holds");
        assert_eq!(seen("", ""), None);
    }

    #[test]
    fn a_rule_set_with_a_bad_region_or_an_empty_matcher_is_refused() {
        let rule = |region: &str, matcher: &str| {
            Rules::parse(
                "x",
                &format!(
                    "[[rule]]\nid = \"r\"\nstate = \"idle\"\npriority = 1\nregion = \"{region}\"\n{matcher}\n"
                ),
            )
        };
        assert!(rule("whole", "contains = [\"a\"]").is_ok());
        assert!(rule("bottom_lines(0)", "contains = [\"a\"]").is_err());
        assert!(rule("after_last_prompt_marker", "contains = [\"a\"]").is_err());
        assert!(rule("whole", "").is_err());
        assert!(rule("whole", "contains = [\"\"]").is_err());
        assert!(rule("whole", "regex = \"a\"").is_err());
        assert!(rule("whole", "not = {}").is_err());
    }

    fn reported(state: Option<State>) -> Reported {
        Reported {
            state,
            ..Reported::default()
        }
    }

    #[test]
    fn full_hooks_win_once_they_report_and_the_screen_wins_over_any_other() {
        let blocked = Seen {
            state: ScreenState::Blocked,
            rule: "codex/action-required".into(),
        };
        let idle = reported(Some(State::Idle));
        let m = merge(Authority::Full, &idle, Some(&blocked));
        assert_eq!(
            (m.state, m.source, m.rule),
            (AgentState::Idle, Source::Hook, None)
        );
        let m = merge(
            Authority::Full,
            &reported(Some(State::Started)),
            Some(&blocked),
        );
        assert_eq!(
            (m.state, m.source, m.rule.as_deref()),
            (
                AgentState::Attention,
                Source::Screen,
                Some("codex/action-required")
            )
        );
        for authority in [Authority::Partial, Authority::Session, Authority::None] {
            let m = merge(authority, &idle, Some(&blocked));
            assert_eq!((m.state, m.source), (AgentState::Attention, Source::Screen));
            let m = merge(authority, &idle, None);
            assert_eq!((m.state, m.source), (AgentState::Idle, Source::Hook));
        }
        for authority in [Authority::Full, Authority::Partial, Authority::None] {
            let m = merge(authority, &reported(None), None);
            assert_eq!((m.state, m.source), (AgentState::Unknown, Source::None));
            let m = merge(authority, &reported(Some(State::Started)), None);
            assert_eq!((m.state, m.source), (AgentState::Unknown, Source::None));
        }
    }
}
