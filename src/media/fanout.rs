//! Fanout batching policy and retained result-codec SQL spellings.

use std::time::Duration;

pub const SECOND_PASS: Duration = Duration::from_secs(60);
pub const CHUNK: i64 = 1_000;

/// `indexed by` is required: with a rowid range the planner could otherwise walk every release between two rows of the title.
pub const UPDATE: &str = "update media_release indexed by media_release_title
   set tvdb_id = iif(tvdb_from_tag = 1, tvdb_id, ?4), tvmaze_id = ?5, trakt_id = ?6,
       tmdb_id = iif(tmdb_from_tag = 1, tmdb_id, ?7), imdb_id = iif(imdb_from_nfo = 1, imdb_id, ?8), tvrage_id = ?9
 where title_key = ?1 and release_id > ?2 and release_id <= ?3
   and ((tvdb_from_tag = 0 and tvdb_id is not ?4) or tvmaze_id is not ?5 or trakt_id is not ?6
        or (tmdb_from_tag = 0 and tmdb_id is not ?7)
        or (imdb_from_nfo = 0 and imdb_id is not ?8) or tvrage_id is not ?9)";

/// The releases of the chunk named by an AlloCiné id, read again into the word index: it finds them by the title the
/// id decoded to (`word_index::known_as`), which a title with no ids changes no row here for its triggers to see.
pub const REINDEX: &str = "insert into word_queue (release_id)
 select m.release_id from media_release m indexed by media_release_title join releases r on r.id = m.release_id
  where m.title_key = ?1 and m.release_id > ?2 and m.release_id <= ?3
    and (r.name glob 'AC[0-9][0-9][0-9][0-9]*' or r.display_name glob 'AC[0-9][0-9][0-9][0-9]*')
 on conflict (release_id) do update set n = n + 1";
