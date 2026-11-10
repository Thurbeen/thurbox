//! Every `git` invocation thurbox makes, all of them on this machine.
//!
//! Split by concern — `command` builds the process (and scrubs the inherited
//! `GIT_*` that would silently retarget it), `plugin` clones a plugin's working
//! copy, `stderr` keeps what a failure printed readable, `discovery` answers
//! "what is here", `newrepo` makes a directory for a repository that does not
//! exist yet, and `diff` produces diffs and a worktree's stats. Fleet creates
//! and removes the worktrees sessions live in.
//! `git::*` is one flat surface; no caller names a submodule.

mod command;
mod diff;
mod discovery;
mod newrepo;
mod plugin;
mod stderr;

#[cfg(test)]
mod tests;

// One flat `git::*` surface, as before: the split is by file, and no caller
// outside this module names a submodule.
//
// A glob re-export clamps each item to its own visibility rather than widening
// it, which is what makes one line per file enough: `pub` items stay the public
// API they were, and the `pub(super)` helpers a sibling needs ride the same
// glob without becoming anybody else's business. Naming them individually would
// have meant an import list per file, kept in step by hand.
// `command` has no `pub` items — its widest is `pub(crate)` — so this glob
// says so rather than claiming a public surface it does not have.
pub(crate) use command::*;
pub use diff::*;
pub use discovery::*;
pub use newrepo::*;
pub use plugin::*;
pub(crate) use stderr::*;
