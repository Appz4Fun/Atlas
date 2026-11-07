//! What a video spot's subcategory numbers mean, as Spotweb names them (`SpotCategories::$_categories` for
//! category 0). Atlas only keeps video spots, so only those are here.

/// The name of video subcategory `letter` `n`, or None for a number Spotweb doesn't name.
pub fn name(letter: char, n: u16) -> Option<&'static str> {
    let list: &[&str] = match letter {
        'a' => &FORMAT,
        'b' => &SOURCE,
        'c' => &LANGUAGE,
        'd' => &GENRE,
        'z' => &TYPE,
        _ => return None,
    };
    list.get(usize::from(n)).copied().filter(|s| !s.is_empty() && *s != "-")
}

const FORMAT: [&str; 16] = [
    "DivX", "WMV", "MPG", "DVD5", "HD other", "ePub", "Blu-ray", "HD-DVD", "WMVHD", "x264", "DVD9", "PDF", "Bitmap",
    "Vector", "3D", "UHD",
];

const SOURCE: [&str; 14] = [
    "CAM",
    "(S)VCD",
    "Promo",
    "Retail",
    "TV",
    "-",
    "Satellite",
    "R5",
    "Telecine",
    "Telesync",
    "Scan",
    "WEB-DL",
    "WEBRip",
    "HDRip",
];

const LANGUAGE: [&str; 16] = [
    "No subtitles",
    "Dutch subtitles (external)",
    "Dutch subtitles (builtin)",
    "English subtitles (external)",
    "English subtitles (builtin)",
    "-",
    "Dutch subtitles (available)",
    "English subtitles (available)",
    "-",
    "-",
    "English audio/written",
    "Dutch audio/written",
    "German audio/written",
    "French audio/written",
    "Spanish audio/written",
    "Asian audio/written",
];

/// 0 to 60, then the erotic genres from 72 (Atlas skips those spots, so they're left unnamed).
const GENRE: [&str; 61] = [
    "Action",
    "Adventure",
    "Animation",
    "Cabaret",
    "Comedy",
    "Crime",
    "Documentary",
    "Drama",
    "Family",
    "Fantasy",
    "Arthouse",
    "Television",
    "Horror",
    "Music",
    "Musical",
    "Mystery",
    "Romance",
    "Science Fiction",
    "Sport",
    "Short movie",
    "Thriller",
    "War",
    "Western",
    "Erotica (hetero)",
    "Erotica (gay male)",
    "Erotica (gay female)",
    "Erotica (bi)",
    "-",
    "Asian",
    "Anime",
    "Cover",
    "Comicbook",
    "Cartoons",
    "Youth",
    "Business",
    "Computer",
    "Hobby",
    "Cooking",
    "Handwork",
    "Craftwork",
    "Health",
    "History",
    "Psychology",
    "Newspaper",
    "Magazine",
    "Science",
    "Female",
    "Religion",
    "Roman",
    "Biography",
    "Detective",
    "Animals",
    "Humor",
    "Travel",
    "True story",
    "Non-fiction",
    "Politics",
    "Poetry",
    "Fairy tale",
    "Technical",
    "Art",
];

const TYPE: [&str; 5] = ["Movie", "Series", "Book", "Erotica", "Picture"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_by_letter_and_number() {
        assert_eq!(name('a', 9), Some("x264"));
        assert_eq!(name('a', 15), Some("UHD"));
        assert_eq!(name('b', 11), Some("WEB-DL"));
        assert_eq!(name('c', 4), Some("English subtitles (builtin)"));
        assert_eq!(name('d', 20), Some("Thriller"));
        assert_eq!(name('d', 60), Some("Art"));
        assert_eq!(name('z', 1), Some("Series"));
        // unnamed: a placeholder, past the end, an unknown letter
        assert_eq!(name('b', 5), None);
        assert_eq!(name('d', 75), None);
        assert_eq!(name('q', 0), None);
    }
}
