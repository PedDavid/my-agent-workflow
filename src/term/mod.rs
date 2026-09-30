//! Terminal layer. Only kitty for now; `TermRef` is an enum so others can follow.

pub mod kitty;

/// What to run inside a new terminal window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub argv: Vec<String>,
}
