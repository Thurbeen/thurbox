mod command;
mod diff;
mod discovery;
mod mod_repo;
mod newrepo;
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
pub use mod_repo::*;
pub use newrepo::*;
pub(crate) use stderr::*;
