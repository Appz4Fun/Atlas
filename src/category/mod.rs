//! Real Newznab categories, in place of every release being 7000.

pub mod lists;
pub mod rules;
pub mod step;
pub mod table;

pub use rules::{Decision, Input, Options, classify, classify_release, stores};
pub use step::CategoryStep;
pub use table::{
    CatFilter, HASHED, MISC, TABLE, UNCATEGORIZED, caps_xml, is_movie, is_tv, label, parse_filter, root_of,
};
