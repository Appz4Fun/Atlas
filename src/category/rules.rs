//! What category a release's name, group and poster put it in.
//!
//! Ported from newznab-tmux `app/Services/Categorization/Categorizers/{Group,Misc,Xxx,Tv,Movie,Pc,Console,Music,Book}*.php`,
//! `app/Services/Categorization/ReleaseContext.php`, `CategorizationResult.php` and `app/Traits/DetectsHashedNames.php`, GPLv3.

use std::sync::LazyLock;

use regex::bytes::{Regex, RegexBuilder};

use crate::naming::obfuscation::is_obfuscated;
use crate::postprocess::step::ReleaseInfo;

use super::lists::list;
use super::table::{HASHED, MISC, UNCATEGORIZED};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub foreign: bool,
    pub web_dl: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { foreign: true, web_dl: false }
    }
}

pub struct Input<'a> {
    pub name: &'a str,
    pub has_display_name: bool,
    pub group: &'a str,
    pub poster: &'a str,
    pub is_obfuscated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub category: i64,
    pub confidence: u16,
    pub rule: &'static str,
}

const NO_MATCH: Decision = Decision { category: UNCATEGORIZED, confidence: 0, rule: "no_match" };

fn ci(p: &str) -> Regex {
    build(p, true)
}

fn cs(p: &str) -> Regex {
    build(p, false)
}

fn build(p: &str, insensitive: bool) -> Regex {
    RegexBuilder::new(p)
        .case_insensitive(insensitive)
        .unicode(false)
        .size_limit(32 << 20)
        .build()
        .unwrap_or_else(|e| panic!("bad category regex {p}: {e}"))
}

macro_rules! rules {
    ($($field:ident = $kind:ident($pat:expr);)*) => {
        struct Rules { $($field: Regex,)* }
        static R: LazyLock<Rules> = LazyLock::new(|| Rules { $($field: $kind($pat),)* });
    };
}

const PC_SKIP: &str = r"[._ -]S\d{1,3}[._ -]?(E\d|Complete|Full|1080|720|480|2160|WEB|HDTV|BluRay)";
const FOREIGN_TV: &str = r"(danish|flemish|Deutsch|dutch|french|german|hebrew|nl[._ -]?sub|dub(bed|s)?|\.NL|norwegian|swedish|swesub|spanish|Staffel)[._ -]|\(german\)|Multisub";
const PUBLISHERS: &str =
    r"Apress|Addison[._ -]Wesley|Manning|No[._ -]Starch|O(?:'|\xE2\x80\x99)?Reilly|Packt|Pragmatic|Wiley|Wrox";

rules! {
    image = ci(r"\.(jpe?g|png|gif|webp|bmp|tiff?)$");

    // context
    season_token = ci(r"(?:^|[._ -])S\d{1,3}(?:$|[._ -])");
    onlyfans = ci(r"\bOnly[-_ ]?Fans\b");
    adult_marker = ci(r"\b(XXX|Porn|Anal|Brazzers|BangBros|Bangbros|NaughtyAmerica|RealityKings|Tushy|Vixen|Blacked|OnlyFans|MetArt|JoyMii|Creampie|MP4-XXX|PureTaboo|Lady[._ -]?Lyne|TeamSkeet|GirlsWay|EvilAngel|Kink|FakeHub|FakeTaxi|SexArt|Nubiles|Defloration|Deeper|Bellesa|Twistys|Mofos|MissaX|LegalPorno|AnalVids|JAV|Hentai|RoccoSiffredi|DivineBitches|Device[._ -]?Bondage|Hogtied|Wired[._ -]?Pussy|Fucking[._ -]?Machines|Ultimate[._ -]?Surrender|Public[._ -]?Disgrace|Sex[._ -]?And[._ -]?Submission|Bound[._ -]?Gang[._ -]?Bangs|Electro[._ -]?Sluts|Whipped[._ -]?Ass|TS[._ -]?Seduction|Infernal[._ -]?Restraints|Sexually[._ -]?Broken)\b");
    // R4: no Teen
    adult_word = ci(r"\b(Fuck|Fucked|Fucking|Cock|Dick|Pussy|Cum|Cumshot|Blowjob|Handjob|MILF|Lesbian|Threesome|Gangbang|Hardcore|Interracial)\b");
    adult_res = ci(r"\b(720p|1080p|2160p|4k|mp4)\b");

    // misc
    hash_md5 = ci(r#"(?:^|["'\s\[\]/\-])([a-f0-9]{32})(?:["'\s\[\]/.\-]|$)"#);
    hash_sha1 = ci(r#"(?:^|["'\s\[\]/\-])([a-f0-9]{40})(?:["'\s\[\]/.\-]|$)"#);
    hash_sha256 = ci(r#"(?:^|["'\s\[\]/\-])([a-f0-9]{64})(?:["'\s\[\]/.\-]|$)"#);
    hash_generic = ci(r#"(?:^|["'\s\[\]/\-])([a-z0-9]{24,128})(?:["'\s\[\]/.\-]|$)"#);
    year_word = cs(r"\b(19|20)\d{2}\b");
    punctuation = cs(r"^[^a-zA-Z]*[A-Z0-9._\-]{5,}[^a-zA-Z]*$");
    punctuation_ext = ci(r"\.(mkv|avi|mp4|mp3|flac|pdf|epub|exe|iso)$");
    archive = ci(r"\.(zip|rar|7z|tar|gz|bz2|xz|tgz|tbz2|cab|iso|img|dmg|pkg|archive)$");
    dataset = ci(r"\b(sql|csv|dump|backup|dataset|collection)\b");
    media_word = ci(r"\b(movie|tv|show|audio|video|book|game)\b");
    leak = ci(r"\b(leak|breach|data|database)\b");
    dump = ci(r"\b(dump|export|backup)\b");
    strip_ext = ci(r"\.(mkv|avi|mp4|m4v|mpg|mpeg|wmv|flv|mov|ts|vob|iso|divx|par2?|nfo|sfv|nzb|rar|r\d{2,3}|zip|7z|gz|tar|001|msix|msixbundle|appx|appxbundle|apk|xap|ipa|deb|rpm|pkg|dmg|exe|msi)$");
    separators = cs(r"[.\-_\s]+");
    core_token = cs(r"^[A-Za-z0-9+/_=-]+$");
    word3 = ci(r"[a-z]{3,}");
    signal_episode = ci(r"\bS\d{1,3}[._ -]?E\d{1,4}\b");
    signal_season = ci(r"\bS\d{1,3}\b");
    signal_resolution = ci(r"\b(480p|576p|720p|1080[pi]?|2160p|4k|uhd)\b");
    signal_codec = ci(r"\b(x264|x265|h\.?264|h\.?265|hevc|xvid|av1)\b");
    signal_source = ci(r"\b(bluray|bdrip|brrip|hdtv|web[._ -]?dl|web[._ -]?rip|dvdrip|remux)\b");
    signal_audio = ci(r"\b(aac|ac3|ddp|dts|flac|mp3)\b");
    signal_tag = ci(r"\b(proper|repack|internal|limited|complete|dubbed|subbed|readnfo)\b");
    signal_group = cs(r"-[A-Za-z0-9][A-Za-z0-9._-]{1,20}$");
    signal_ext = ci(r"\.(mkv|avi|mp4|mp3|flac|iso|epub|pdf|exe|nzb|rar|7z)$");

    // group
    g_anime = ci(r"alt\.binaries\..*anime");
    // R5: teevee
    g_tv = ci(r"alt\.binaries\..*?(tv|teevee|hdtv|tvseries)");
    g_movie = ci(r"alt\.binaries\..*?(movies?|dvd|bluray|x264)");
    g_xxx = ci(r"alt\.binaries\..*?(erotica|pictures\.erotica|xxx)");
    g_music = ci(r"alt\.binaries\..*?(sounds?|mp3|music|lossless)");
    g_game = ci(r"alt\.binaries\..*?(games?|console|psx|nintendo)");
    g_pc = ci(r"alt\.binaries\..*?(warez|0day|apps?|software)");
    g_book = ci(r"alt\.binaries\..*?(e-?book|ebook|comics?)");

    // xxx
    xxx_word = ci(r"\bXXX\b");
    studios = ci(&format!(r"\b({})\b", list("KNOWN_STUDIOS")));
    adult_keywords = ci(&format!(r"\b({})\b", list("ADULT_KEYWORDS")));
    vr_sites = ci(&format!(r"\b({})\b", list("VR_SITES")));
    vr_generic = ci(list("GENERIC_VR_SITE"));
    vr_devices = ci(&format!(r"\b(?:{})\b", list("VR_DEVICES")));
    movie_after_studio = ci(r"^(?:[A-Za-z]+[.\-_ ])*(?:19|20)\d{2}[.\-_ ](?:720p|1080p|2160p|4K|UHD)");
    movie_source = ci(r"\b(WEB-?DL|WEBRip|BluRay|BDRip|HDRip|HDTV|DVDRip|Remux|PROPER|REPACK|HC|KORSUB)\b");
    vr_res = ci(r"\b(720p|1080p|2160p|4K|VR180|VR360)\b");
    video_mark = ci(r"\b(720p|1080p|2160p|4k|mp4|mkv|avi|wmv)\b");
    jav = cs(r"\b(AV|JAV)\b");
    site_date = ci(r"^[A-Za-z]+[.\-_ ](19|20)?\d{2}[.\-_ ]\d{2}[.\-_ ]\d{2}[.\-_ ][A-Za-z]");
    performer = ci(r"\d{2}[.\-_ ]([a-z]+)[.\-_ ]([a-z]+)[.\-_ ]");
    tv_not_adult = ci(r"\b(S\d{1,2}E\d{1,2}|Episode|Season|HDTV|PDTV)\b");
    photo_pack = ci(r"\b(photo(?:set)?s?|image(?:set)?s?|pics?|wallpapers?|collections?|packs?)\b");
    video_hint = ci(r"\b(mp4|mkv|mov|wmv|avi|webm|h\.?264|x264|h\.?265|x265)\b");
    vr_180_360 = ci(r"\bVR(?:180|360)\b");
    vr_pattern = ci(&format!(r"\b({})\b|{}|\bVR(?:180|360)\b|\b(?:5K|6K|7K|8K)\b.*\bVR\b|\b(?:{})\b", list("VR_SITES"), list("GENERIC_VR_SITE"), list("VR_DEVICES")));
    x_uhd = ci(r"\b(2160p|4k|UHD|Ultra[._ -]?HD)\b");
    // R4: no Teen
    x_uhd_marker = ci(r"\b(Hardcore|Porn|Sex|Anal|Creampie|MILF|Lesbian|Interracial)\b");
    x_uhd_group = ci(r"XXX.+2160p[\w\-.]+M[PO][V4]-(KTR|GUSH|FaiLED|SEXORS|hUSHhUSH|YAPG|WRB|NBQ|FETiSH)");
    x_pack_first = ci(r"^(Complete|Pack|Collection|Anthology|Siterip|SiteRip)\b");
    x_tv = ci(r"\b(S\d{1,2}E\d{1,2}|S\d{1,2}|Season\s\d{1,2})\b");
    x_hd = ci(r"\b(720p|1080p|2160p|HD|4K)\b");
    x_studio_title = ci(&format!(r"^(?:(?:{})\.|{}[.\-_ ])([A-Z][a-z]+).*?(720p|1080p|2160p|HD|4K)", list("KNOWN_STUDIOS"), list("JACQUIE_ET_MICHEL")));
    x_studio_date = ci(&format!(r"^({})[.\-_ ](19|20)?\d{{2}}[.\-_ ]\d{{2}}[.\-_ ]\d{{2}}", list("KNOWN_STUDIOS")));
    x_date4 = ci(r"^([A-Z][a-zA-Z0-9]+)[.\-_ ](19|20)\d{2}[.\-_ ]\d{2}[.\-_ ]\d{2}[.\-_ ]");
    x_not_docs = ci(r"\b(S\d{2}E\d{2}|Documentary|Series)\b");
    x_date2 = ci(r"^([A-Z][a-zA-Z0-9]+)\.(\d{2})\.(\d{2})\.(\d{2})\..*?(720p|1080p|2160p|HD|4K)");
    x_jav_date = ci(&format!(r"^({})[.\-_ ](\d{{6}})", list("KNOWN_STUDIOS")));
    x_studio_xxx = ci(&format!(r"^({})[.\-_ ].*\bXXX\b.*?(720p|1080p|2160p|HD|4K)", list("KNOWN_STUDIOS")));
    x_word_res = ci(r"\b(XXX|MILF|Anal|Sex|Porn)[._ -]+(720p|1080p|2160p|HD|4K)\b");
    x_res_word = ci(r"\b(720p|1080p|2160p|HD|4K)[._ -]+(XXX|MILF|Anal|Sex|Porn)\b");
    x_pack = ci(r"[ .]PACK[ .]");
    x_clip_poster = ci(r"anon@y[.]com|@md-hobbys[.]com|oz@lot[.]com");
    x_clip_name = ci(r"(iPT\sTeam|KLEENEX)");
    x_sd = ci(r"SDX264XXX|XXX\.HR\.");
    x_ep = ci(r"\b(S\d{1,2}E\d{1,2})\b");
    web = ci(r"web[._ -]dl|web-?rip");
    x_webdl_word = ci(r"\b(XXX|Porn|Adult|JAV|Hentai)\b");
    x_x265 = ci(r"\b(x265|hevc)\b");
    x_x264 = ci(r"\b((x|h)[\.\-_ ]?264|AVC)\b");
    x_not264 = ci(r"\bwmv\b|S\d{1,2}E\d{1,2}|\d+x\d+");
    x_adult264 = ci(&format!(r"\bXXX\b|a\.b\.erotica|BangBros|{}|Cum|Defloration|Err?oticax?|JoyMii|MetArt|Nubiles|Porn|SexArt|Tushy|Vixen|JAV|Brazzers|NaughtyAmerica|RealityKings", list("JACQUIE_ET_MICHEL")));
    x_xvid = ci(r"(b[dr]|dvd)rip|detoxication|divx|nympho|pornolation|swe6|tesoro|xvid");
    x_imageset = ci(r"IMAGESET|PICTURESET|ABPEA");
    x_modern = ci(r"\b(720p|1080p|2160p|x264|x265|h264|h265|hevc|XviD|MP4-|\.mp4)[._ -]");
    x_wmv = ci(r"\b(WMV|Windows\s?Media\s?Video)\b|\.wmv$|[._ -]wmv[._ -]");
    x_dvd = ci(r"dvdr[^i]|dvd[59]");
    x_other = ci(r"[._ -]Brazzers|Creampie|[._ -]JAV[._ -]|North\.Pole|She[._ -]?Male|Transsexual|OLDER ANGELS");

    // tv
    tosho = ci(r"animetosho\.org");
    anime_groups = ci(list("ANIME_GROUPS_REGEX"));
    anime_hash = ci(r"^\[.+\].*\d{2,3}.*\[[a-fA-F0-9]{8}\]");
    tv_ep = ci(r"[._ -]s\d{1,3}[._ -]?(e|d(isc)?)\d{1,3}([._ -]|$)");
    tv_e = ci(r"[._ -]E\d{1,4}[._ -]");
    tv_complete = ci(r"[._ -]S\d{1,3}[._ -]?(Complete|COMPLETE|Full|FULL)");
    tv_pack = ci(r"[._ -]S\d{1,3}[._ -](480p|720p|1080[pi]|2160p|4K|UHD|WEB|HDTV|BluRay|NF|AMZN|DSNP|ATVP|HMAX)");
    tv_episode_word = ci(r"\b(Episode|Ep)[._ -]?\d{1,4}\b");
    tv_source = ci(r"\b(HDTV|PDTV|DSR|TVRip|SATRip|DTHRip)\b");
    tv_daily = ci(r"[._ -](19|20)\d{2}[._ -]\d{2}[._ -]\d{2}[._ -]");
    sports = ci(list("SPORTS_MARKER_REGEX"));
    sports_context = ci(list("SPORTS_EVENT_CONTEXT_REGEX"));
    anime_word = ci(r"[._ -]Anime[._ -]");
    anime_no = ci(r"[._ ]no[._ ]");
    anime_roman = ci(r"[._ ](I{1,3}|IV|V|VI{0,3}|IX|X)[._ ]?E\d{1,4}[._ ]");
    anime_e = ci(r"[._ ]E\d{1,4}[._ ]");
    anime_se = ci(r"[._ ]S\d{1,3}[._ ]?E\d");
    anime_bd = ci(r"\b(BluRay|BD|BDRip)\b");
    anime_s = ci(r"\bS\d{1,3}\b");
    anime_roman_strict = ci(r"[._ ](I{1,3}|IV|V|VI{0,3}|IX|X)[._ ]E\d{1,4}[._ ]");
    tv_doc = ci(r"\b(Documentary|Docu[._ -]?Series|DOCU)\b");
    tv_foreign = ci(FOREIGN_TV);
    tv_x265 = ci(r"(S\d+).*(x265).*(rmteam|MeGusta|HETeam|PSA|ONLY|H4S5S|TrollHD|ImE)");
    tv_se = ci(r"S\d+[._ -]?E\d+");
    res2160 = ci(r"2160p");
    tv_uhd_season = ci(r"[._ -]S\d+[._ -].*2160p");
    tv_uhd_stream = ci(r"(S\d+).*(2160p).*(Netflix|Amazon|NF|AMZN).*(TrollUHD|NTb|VLAD|DEFLATE|POFUDUK|CMRG)");
    tv_hd = ci(r"1080([ip])|720p|bluray");
    tv_sd = ci(r"(360|480|576)p|Complete[._ -]Season|dvdr(ip)?|dvd5|dvd9|\.pdtv|SD[._ -]TV|TVRip|NTSC|BDRip|hdtv|xvid");
    tv_sd_codec = ci(r"(([HP])D[._ -]?TV|DSR|WebRip)[._ -]x264");

    // movie
    m_skip_tv = ci(r"[._ -]S\d{1,3}[._ -]?(E\d|D\d|Complete|Full|1080|720|480|2160|WEB|HDTV|BluRay|NF|AMZN)");
    m_skip_anime = ci(r"(?:^|[.\-_ \[])(URANiME|ANiHLS|HaiKU|ANiURL|SkyAnime|Erai-raws|LostYears|Vodes|SubsPlease|Judas|Ember|YuiSubs|ASW|Tsundere-Raws|Anime-Raws)(?:[.\-_ \]]|$)");
    m_looks = ci(r"[._ -]AVC|[BH][DR]RIP|(Bluray|Blu-Ray)|BD[._ -]?(25|50)?|\bBR\b|Camrip|[._ -]\d{4}[._ -].+(720p|1080p|Cam|HDTS|2160p)|DIVX|[._ -]DVD[._ -]|DVD-?(5|9|R|Rip)|Untouched|VHSRip|XVID|[._ -](DTS|TVrip|webrip|WEBDL|WEB-DL)[._ -]|\b(2160)p\b.*\b(Netflix|Amazon|NF|AMZN|Disney)\b");
    m_foreign = ci(r"(danish|flemish|Deutsch|dutch|french|german|heb|hebrew|nl[._ -]?sub|dub(bed|s)?|\.NL|norwegian|swedish|swesub|spanish|Staffel)[._ -]|\(german\)|Multisub");
    m_foreign2 = ci(r"(720p|1080p|AC3|AVC|DIVX|DVD(5|9|RIP|R)|XVID)[._ -](Dutch|French|German|ITA)|\(?(Dutch|French|German|ITA)\)?[._ -](720P|1080p|AC3|AVC|DIVX|DVD(5|9|RIP|R)|WEB(-DL|-?RIP)|HD[._ -]|XVID)");
    m_x265 = ci(r"(\w+[\.-_\s]+).*(x265).*(Tigole|SESKAPiLE|CHD|IAMABLE|THREESOME|OohLaLa|DEFLATE|NCmt)");
    m_uhd_tv = ci(r"(S\d+).*(2160p).*(Netflix|Amazon|NF|AMZN).*(TrollUHD|NTb|VLAD|DEFLATE|CMRG)");
    m_uhd = ci(r"\b(UHD|Ultra[._ -]HD|4K)\b");
    m_hdr = ci(r"\b(HDR|HDR10|HDR10\+|Dolby[._ -]?Vision)\b");
    m_hevc = ci(r"\b(HEVC|H\.?265|x265)\b");
    m_br = ci(r"\b(BR|BluRay|Blu[._ -]?Ray)\b");
    m_3d = ci(r"[._ -]3D\s?[\.\-_\[ ](1080p|(19|20)\d\d|AVC|BD(25|50)|Blu[._ -]?ray|CEE|Complete|GER|MVC|MULTi|SBS|H(-)?SBS)[._ -]");
    m_bluray = ci(r"bluray-|[._ -]bd?[._ -]?(25|50)|blu-ray|Bluray\s-\sUntouched|[._ -]untouched[._ -]");
    m_secret = ci(r"SecretUsenet\.com$");
    m_dvd = ci(r"(dvd\-?r|[._ -]dvd|dvd9|dvd5|[._ -]r5)[._ -]");
    m_hd = ci(r"720p|1080p|AVC|VC1|VC-1|web-dl|wmvhd|x264|XvidHD|bdrip");
    m_sd = ci(r"(divx|dvdscr|extrascene|dvdrip|\.CAM|HDTS(-LINE)?|vhsrip|xvid(vd)?)[._ -]");
    m_cam = ci(r"[._ -]cam[._ -]");

    // pc
    pc_skip = ci(PC_SKIP);
    pc_ios = ci(r"[^a-z0-9](IPHONE|ITOUCH|IPAD)[._ -]");
    pc_android = ci(r"[._ -]?(ANDROID)[._ -]");
    pc_phone = ci(r"[^a-z0-9](symbian|xscale|wm5|wm6)[._ -]");
    pc_mac = ci(r"(\b|[._ -])mac([\.\s])?osx(\b|[\-_. ])");
    pc_macos = ci(r"\b(Mac\s?OS\s?X|macOS)\b");
    pc_console = ci(r"\b(PS5|PS4|PS3|PlayStation|PS(Vita|V)\b|Xbox\s?(Series|One|360)|XBOX(ONE|360|SERIES|SX|SS)?|XSX|XSS|XBSX|NSW|Switch|WiiU|Wii|3DS|NDS|PSP|PSV(ita)?|GameCube|NGC|CUSA\d{5}|XCI|NSP|PKG)\b");
    pc_tv = ci(r"\b(S\d{1,4}[._ -]?E\d{1,4}|S\d{1,4}[._ -]?D\d{1,4}|\d{1,2}x\d{2,3}|Season[._ -]?\d{1,3}|Episode[._ -]?\d{1,4}|HDTV|PDTV|DSR|WEB[._ -]?DL|WEB[._ -]?RIP|TVRip)\b");
    pc_group = ci(&format!(r"[-.]({})$", list("PC_GROUPS")));
    pc_keyword = ci(list("PC_KEYWORDS"));
    pc_poster = ci(r"<PC@MASTER\.RACE>");
    pc_iso = ci(r"[._ -]([a-zA-Z]{2,10})?iso[ _.-]|[\-. ]([a-z]{2,10})?iso$");
    pc_training = ci(r"[._ -](DYNAMiCS|INFINITESKILLS|UDEMY|kEISO|PLURALSIGHT|DIGITALTUTORS|TUTSPLUS|OSTraining|PRODEV|CBT\.Nuggets|COMPRISED)");
    pc_msix = ci(r"\.(msix|msixbundle|appx|appxbundle|msi)$");
    pc_explicit = ci(r"[._ -]exe$|[._ -](utorrent|Virtualbox)[._ -]|\b0DAY\b|incl.+crack| DRM$|>DRM<");
    pc_system = ci(r"[._ -]((32|64)bit|converter|i\d86|key(gen|maker)|freebsd|GAMEGUiDE|hpux|irix|linux|multilingual|Patch|Pro v\d{1,3}|portable|regged|software|solaris|template|unix|win2kxp2k3|win64|win(2k|32|64|all|dows|nt(2k)?(xp)?|xp)|win9x(me|nt)?|x(32|64|86))[._ -]");
    pc_software = ci(r"\b(Adobe|auto(cad|desk)|-BEAN|Cracked|Cucusoft|CYGNUS|Divx[._ -]Plus|\.(deb|exe)|DIGERATI|FOSI|-FONT|Key(filemaker|gen|maker)|Lynda\.com|lz0|MULTiLANGUAGE|Microsoft\s*(Office|Windows|Server)|MultiOS|-(iNViSiBLE|SPYRAL|SUNiSO|UNION|TE)|v\d{1,3}.*?Pro|[._ -]v\d{1,3}[._ -]|\(x(64|86)\)|Xilisoft)\b");

    // console
    c_ps4_first = ci(r"^PS4[_\.\-]");
    c_cusa = ci(r"CUSA\d{5}");
    c_duplex = ci(r"\.PS4-DUPLEX$");
    c_ps4 = ci(r"\bPS4\b|PlayStation\s*4");
    c_ps3 = ci(r"\bPS3\b|PlayStation\s*3");
    c_vita = ci(r"\bPS\s?Vita\b|PSV(ita)?\b");
    c_psp = ci(r"\bPSP\b|PlayStation\s*Portable");
    c_xone = ci(r"\b(XboxOne|XBOX\s*One|XBONE|XB1|Xbox\s*Series[._ -]?[SX]|XSX|XSS)\b");
    c_x360 = ci(r"\b(Xbox360|XBOX360|X360)\b");
    c_xbox = ci(r"\bXBOX\b");
    c_xbox_not = ci(r"\b(XBOX\s?360|XBOX\s?ONE|Series)\b");
    c_wiiu = ci(r"\bWii\s*U\b|WiiU");
    c_wii = ci(r"\bWii\b");
    c_wiiu_word = ci(r"WiiU");
    c_3ds = ci(r"\b3DS\b|Nintendo\s*3DS");
    c_nds = ci(r"\bNDS\b|Nintendo\s*DS");
    c_retro = ci(r"\b(PS[12X]|PS2|SNES|NES|SEGA|GB[AC]?|Dreamcast|Saturn|Atari|N64)\b");
    c_region = ci(r"\b(EUR|JP|JPN|NTSC|PAL|USA|ROM)\b");

    // music
    a_foreign = ci(&format!(r"(?:^|[\s\.\-_])(?:{}|{})(?:$|[\s\.\-_])", list("FOREIGN_LANGUAGES"), list("LANGUAGE_CODES")));
    a_book = ci(r"(?:^|[^a-zA-Z0-9])(?:Audiobook|Audio\s*Book|Talking\s*Book|ABEE|Audible)");
    a_book_word = ci(r"\b(?:Unabridged|Abridged|Narrated|Narrator|MP3|M4A|M4B|AAC|Read\s+By|Tantor|Blackstone|Brilliance|GraphicAudio|Penguin|Audible)\b");
    a_book_len = ci(r"\d+\s*CDs|\d+\s*Hours|Spoken\s+Word");
    a_book_ext = ci(r"\.(mp3|m4a|m4b|aac|flac|ogg|wma)$");
    a_book_tag = ci(r"(?:[\(_\[])(?:Audiobook|AB|Unabridged)(?:[\)_\]])");
    a_book_read = ci(r"Read\s+By\s+[A-Z][a-z]+\s+[A-Z][a-z]+");
    a_book_old = ci(r"(Audiobook|Audio.?Book)");
    a_pod = ci(r"(?:^|[^a-zA-Z0-9])(?:Podcast|Pod[._ -]?cast|Pod[._ -]Show)");
    a_pod_net = ci(r"\b(?:NPR|BBC[._ -]Sounds|Gimlet|Wondery|Stitcher|iHeart[._ -]?Radio|Joe[._ -]Rogan|RadioLab|Serial)\b");
    a_pod_ep = ci(r"\b(?:Podcast|Episode|EP?[._ -]?\d+|Show)\b");
    a_pod_any = ci(r"podcast");
    a_mv = ci(r"(?:^|[^a-zA-Z0-9])(?:Music\s*Video|Concert|Live\s*Show|Tour|Festival|MV|MTV)|\b(?:MVID|MVid)\b");
    a_mv_res = ci(r"\b(?:720p|1080[pi]|2160p|BDRip|BluRay|DVDRip|HDTV|WebRip|WEB-DL|x264|x265)\b");
    a_mv_word = ci(r"\b(?:Live|Unplugged|Acoustic|World\s*Tour|in\s*Concert|Official\s*Video|Bootleg|Remastered)\b");
    a_mv_ext = ci(r"\.(mkv|mp4|avi|ts|m2ts|mpg|mpeg|mov|wmv|vob|m4v)$");
    a_mv_artist = ci(r"^[A-Z0-9][A-Za-z0-9\.\s\&'\(\)\-]+\s+\-\s+[A-Z0-9][A-Za-z0-9\.\s\&'\(\)\-]+.*?\b(720p|1080[pi]|2160p|Bluray|x264|x265)\b");
    a_ll = ci(r"(?:^|[^a-zA-Z0-9])(?:FLAC|APE|WAV|ALAC|DSD|DSF|AIFF|PCM|Lossless)|\b(?:FLAC|APE|WAV|ALAC|DSD|DSF|AIFF|PCM)\b");
    a_ll_word = ci(r"\b(?:24[Bb]it|96kHz|192kHz|Hi[- ]?Res|HD[- ]?Tracks|Vinyl[- ]?Rip|CD[- ]?Rip|WEB[- ]?Rip|HDtracks|Qobuz|Tidal|MQA|SACD)\b");
    a_ll_ext = ci(r"\.(flac|ape|wav|aiff|dsf|dff|m4a|tak)$");
    a_flac = ci(r#"\[(19|20)\d\d\][._ -]\[FLAC\]|([\(\[])flac([\)\]])|FLAC\-(19|20)\d\d\-[a-z0-9]{1,12}|\.flac"|(19|20)\d\d\sFLAC|[._ -]FLAC.+(19|20)\d\d[._ -]| FLAC$"#);
    a_flac2 = ci(r"\d{3,4}kbps[._ -]FLAC|\[FLAC\]|\(FLAC\)|FLACME|FLAC[._ -]\d{3,4}(kbps)?|WEB[._ -]FLAC");
    a_ll_other = ci(r"\b(?:APE|Monkey's[._ -]Audio|WavPack|WV|TAK|TTA|ALAC|Apple[._ -]Lossless)\b|\.(ape|wv|tak|tta)$");
    a_mp3 = ci(r"(?:^|[^a-zA-Z0-9])(?:MP3|320kbps|256kbps|192kbps|128kbps|CBR|VBR)|\b(?:MP3)\b|[\._-](?:MP3)[\._-]|\.mp3$");
    a_mp3_rate = ci(r"\b(?:320|256|192|128)[._-]?kbps|\b(?:320|256|192|128)[._-]?K|\((?:320|256|192|128)\)|\[(?:320|256|192|128)\]|V0|V2|VBR");
    a_mp3_src = ci(r"\b(?:CD[._-]?Rip|Web[._-]?Rip|WEB|iTunes|AmazonRip|Spotify[._-]?Rip|MP3\s*\-\s*\d{3}kbps)\b");
    a_mp3_more = ci(r#"\.(m3u|mp3)"|rip(?:192|256|320)|[._-]FM[._-].+MP3"#);
    a_mp3_scene = ci(r"^[a-zA-Z0-9]{1,12}[._-](19|20)\d\d[._-][a-zA-Z0-9]{1,12}$|[a-z0-9]{1,12}\-(19|20)\d\d\-[a-z0-9]{1,12}");
    a_mp3_bit = ci(r"[\.\-\(\[_ ]\d{2,3}k[\.\-\)\]_ ]|\((192|256|320)\)|(320|cd|eac|vbr)[._-]+mp3|(cd|eac|mp3|vbr)[._-]+320");
    a_other = ci(r"(?:^|[^a-zA-Z0-9])(?:Compilation|Various[._ -]Artists|OST|Soundtrack|B-Sides|Greatest[._ -]Hits|Anthology)|\b(?:VA|V\.A|Bonus[._ -]Track|Discography|Box[._ -]Set)\b");
    a_album = ci(r"(?:\d)[._ -](?:CD|Albums|LP)[._ -](?:Set|Compilation)|CD[._ -](Collection|Box|SET)|(\d)-?CD[._ -]");
    a_album2 = ci(r"Vinyl[._ -](?:24[._ -]96|2496|Collection|RIP)|WEB[._ -](?:Single|Album)|EP[._ -]\d{4}|\bEP\b.+(?:19|20)\d\d|Live[._ -](?:at|At|@)");
    a_dj = ci(r"\b(?:Ministry[._ -]of[._ -]Sound|Hed[._ -]Kandi|Cream|Fabric[._ -]Live|Ultra[._ -]Music)\b");
    a_dj2 = ci(r"\b(?:DJ[._ -]Mix|Mixed[._ -]By|Tiesto[._ -]Club|Radio[._ -]Show|Club[._ -]Hits)\b");

    // book
    b_ps4 = ci(r"\.PS4-[A-Z0-9]+$");
    b_console = ci(r"\b(?:PS[1-5]|PlayStation|Xbox|Switch|Nintendo|Wii|3DS|GameCube)\b");
    b_game_id = ci(r"\b(?:PPSA\d{4,6}|CUSA\d{5}|XCI|NSP|PKG)\b");
    b_soft_version = ci(r"\bv?\d+(?:\.\d+){1,4}\b.*\b(?:Multilingual|x64|x86|Portable|Setup|Patch|Install(?:er)?|Crack(?:ed)?|Keygen|Serial|Regged|Pre-?Activated)\b");
    // R6: no CBR
    b_music = ci(r"\b(?:WEB[\.\-_ ]?FLAC|FLAC|MP3|M4A|M4B|AAC|OGG|ALAC|WAV|LOSSLESS|320kbps|256kbps|192kbps|128kbps|VBR|discography|FALCON|OST|Soundtrack|Vinyl[._ -]?Rip|CD[._ -]?Rip)\b");
    b_course = ci(r"\b(?:Udemy|Pluralsight|Lynda\.?com|Coursera|LinkedIn[._ -]?Learning|Skillshare|MasterClass|CBT[._ -]?Nuggets|Digital-?Tutors)\b");
    b_res = ci(r"\b(?:720p|1080p|2160p|4k)\b");
    b_video = ci(r"\b(?:WEB[._ -]?DL|WEBRip|HDTV|BluRay|BDRip|DVDRip|x264|x265|HEVC|HDR|Remux|AVC)\b");
    b_distro = ci(r"\b(?:Ubuntu|Debian|Fedora|Kali|Arch(?:Linux)?|CentOS|OpenSUSE|LinuxMint)\b.*\b(?:ISO|img|amd64|x86_64|installer)\b");
    b_image = ci(r"\b(?:ISO|IMG|DMG|bootable)\b");
    b_font = ci(r"\b(?:Font(?:s)?|Typeface|OpenType|TrueType)\b");
    b_font_ext = ci(r"\.(?:otf|ttf|woff2?)$");
    b_movie = ci(r"\b(19|20)\d{2}\b.*\b(1080p|720p|2160p|BluRay|WEB-DL|BDRip|DVDRip)\b");
    b_comic = ci(r"\b(?:CBR|CBZ|C2C)\b|\.(?:cbr|cbz)$");
    b_publisher = ci(r"\b(?:Marvel|DC[._ -]Comics|Image[._ -]Comics|Dark[._ -]Horse|IDW)\b");
    b_comic_word = ci(r"\b(?:Comics?|Annual|Issue|Vol|TPB)\b");
    b_manga = ci(r"\b(?:Manga|Manhwa|Manhua|Webtoon)\b");
    b_tech_pub = ci(&format!(r"\b({PUBLISHERS})\b"));
    b_subject = ci(r"\b(Programming|Python|JavaScript|Java|Database|Linux|DevOps|Machine[._ -]Learning|Data[._ -]Science)\b");
    b_signal = ci(r"\b(?:Book|Guide|Tutorial|Learn|Handbook|Cookbook|Reference)\b");
    b_mag_title = ci(r"\b(Forbes|Fortune|GQ|National[._ -]Geographic|Newsweek|Vogue|Wired|The[._ -]?Economist|New[._ -]?Yorker|Scientific[._ -]?American|Popular[._ -]?Mechanics|Cosmopolitan|Elle|Esquire|Vanity[._ -]?Fair|Rolling[._ -]?Stone|Entertainment[._ -]?Weekly|People|Playboy)\b");
    b_issue = ci(r"(?:^|[._ -])Issue[._ -]?\d{1,4}(?:$|[._ -,])");
    b_date = ci(r"\b(?:19|20)\d{2}\b|(?:^|[._ -])(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[a-z]*[._ -]?(?:19|20)?\d{2}\b");
    b_freq = ci(r"[._ -](Monthly|Weekly|Quarterly|Annual)[._ -]");
    b_issue_title = ci(r"\b([A-Z][a-zA-Z]+(?:\s+[A-Z][a-zA-Z]+)*)\s*-\s*Issue\s+\d+");
    b_mcn = ci(r"\bMCN[._ -](?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec|January|February|March|April|June|July|August|September|October|November|December)[._ -]\d{1,2}[._ -](?:19|20)\d{2}\b");
    b_magazine = ci(r"\bMAGAZINE\b");
    b_strict_end = ci(r"\.(EPUB|MOBI|AZW\d?|FB2|DJVU|LIT)$");
    b_strict = ci(r"\b(EPUB|MOBI|AZW\d?|FB2|DJVU|LIT)\b");
    b_pdf_end = ci(r"\.PDF$");
    b_pdf = ci(r"\bPDF\b");
    b_platform = ci(r"\b(E-?book|Kindle|Kobo|Nook)\b");
    b_year = cs(r"\b(19|20)\d{2}\b");
    b_corroborate = ci(r"\b(?:ISBN(?:-1[03])?|Edition|Ed\.|Author|Novel|Volume|Vol\.|Chapter|Press|Paperback|Hardcover|E-?book|Kindle|Kobo)\b");
    b_corroborate_pub = ci(&format!(r"\b(?:{PUBLISHERS})\b"));
    b_corroborate_ext = ci(r"\.(?:epub|mobi|azw\d?|fb2|djvu|lit|pdf)$");
    b_soft = ci(r"\b(?:Setup|Install(?:er)?|Portable|Crack(?:ed)?|Keygen|Serial|Regged|Pre-?Activated|Patch(?:ed)?|NFO|ReadNFO|Win(?:dows)?|macOS|Linux|x64|x86|amd64|AIO|Retail|Incl)\b");
    b_soft_vendor = ci(r"\b(?:Adobe|Foxit|Autodesk|AutoCAD|Corel|Microsoft[._ -]Office|Office(?:365)?|Visual[._ -]Studio|IntelliJ|PyCharm|VMware|VirtualBox)\b");
    b_soft_ext = ci(r"\.(?:exe|msi|dmg|pkg)$");
}

fn m(re: &Regex, text: &str) -> bool {
    re.is_match(text.as_bytes())
}

fn hit(category: i64, confidence: u16, rule: &'static str) -> Option<Decision> {
    Some(Decision { category, confidence, rule })
}

fn contains_ci(text: &str, word: &str) -> bool {
    text.to_ascii_lowercase().contains(word)
}

struct Ctx<'a> {
    name: &'a str,
    group: &'a str,
    poster: &'a str,
    foreign: bool,
    web_dl: bool,
}

impl Ctx<'_> {
    fn standalone_season(&self) -> bool {
        m(&R.season_token, self.name)
    }

    fn has_only_fans(&self) -> bool {
        self.name.starts_with("OF.") || m(&R.onlyfans, self.name)
    }

    fn has_adult_markers(&self) -> bool {
        self.has_only_fans()
            || m(&R.adult_marker, self.name)
            || (m(&R.adult_word, self.name) && m(&R.adult_res, self.name))
    }
}

type Pipe = fn(&Ctx<'_>) -> Option<Decision>;

/// the same test as VISIBLE_RELEASE (src/search.rs), negated
pub(crate) fn is_hidden(name: &str, has_display_name: bool, is_obfuscated: bool) -> bool {
    !has_display_name
        && (is_obfuscated || (name.chars().count() >= 16 && name.bytes().all(|b| b.is_ascii_alphanumeric())))
}

/// CategorizationResult.php:178-192
fn protected(d: &Decision) -> bool {
    d.category == HASHED || (d.category == MISC && (locks_rule(d.rule) || d.rule == "group_only_low_signal"))
}

fn locks_rule(rule: &str) -> bool {
    ["hash_", "obfuscated_", "gibberish_"].iter().any(|p| rule.starts_with(p))
}

fn locks(d: &Decision) -> bool {
    d.category == HASHED || locks_rule(d.rule)
}

const ALL_PIPES: [Pipe; 8] = [group_pipe, xxx_pipe, tv_pipe, movie_pipe, pc_pipe, console_pipe, music_pipe, book_pipe];
const IMAGE_PIPES: [Pipe; 2] = [group_xxx_pipe, xxx_pipe];

fn group_xxx_pipe(c: &Ctx<'_>) -> Option<Decision> {
    group_pipe(c).filter(|d| d.rule == "group_name_xxx")
}

/// CategorizationResult.php:123-141
fn overrides(new: &Decision, best: &Decision) -> bool {
    if new.category != MISC && new.category != HASHED && protected(best) && new.confidence < 600 {
        return false;
    }
    new.confidence > best.confidence
        || (new.confidence == best.confidence && new.category != MISC && best.category == MISC)
}

pub fn classify(i: &Input<'_>, o: &Options) -> Decision {
    if i.name.trim().is_empty() {
        return Decision { category: HASHED, confidence: 1000, rule: "unnamed" };
    }
    if is_hidden(i.name, i.has_display_name, i.is_obfuscated) {
        return Decision { category: HASHED, confidence: 1000, rule: "hidden" };
    }
    let c = Ctx { name: i.name, group: i.group, poster: i.poster, foreign: o.foreign, web_dl: o.web_dl };
    let (low_signal, misc) = misc_pipe(&c);
    let mut best = NO_MATCH;
    if let Some(found) = misc {
        if locks(&found) {
            return found;
        }
        if overrides(&found, &best) {
            best = found;
        }
    }
    // R12: an image is not a video, a book or a game
    let pipes: &[Pipe] = if m(&R.image, c.name) { &IMAGE_PIPES } else { &ALL_PIPES };
    for pipe in pipes {
        if best.confidence >= 950 {
            break;
        }
        if let Some(found) = pipe(&c)
            && overrides(&found, &best)
        {
            best = found;
        }
    }
    if low_signal && best.rule.starts_with("group_name_") {
        return Decision { category: MISC, confidence: 650, rule: "group_only_low_signal" };
    }
    best
}

/// Pure, no I/O. `name` is coalesce(display_name, name).
pub fn classify_release(release: &ReleaseInfo, options: &Options) -> Decision {
    let input = Input {
        name: release.display_name.as_deref().unwrap_or(&release.name),
        has_display_name: release.display_name.is_some(),
        group: &release.group,
        poster: release.poster.as_deref().unwrap_or(""),
        is_obfuscated: release.is_obfuscated,
    };
    classify(&input, options)
}

/// False when a missing row already says it.
pub fn stores(d: &Decision) -> bool {
    !matches!(d.rule, "no_match" | "hidden" | "unnamed" | "obfuscated_name")
}

// ---------------------------------------------------------------- misc

fn strip_extensions(name: &str) -> String {
    let trimmed = name.trim();
    match R.strip_ext.find(trimmed.as_bytes()) {
        Some(found) => trimmed[..found.start()].to_string(),
        None => trimmed.to_string(),
    }
}

fn core_name(cleaned: &str) -> String {
    let bytes = R.separators.replace_all(cleaned.as_bytes(), &b""[..]);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn max_letters(s: &str) -> usize {
    let (mut longest, mut run) = (0, 0);
    for b in s.bytes() {
        run = if b.is_ascii_alphabetic() { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

fn normal_vowel_ratio(s: &str) -> bool {
    let letters: Vec<u8> = s.bytes().filter(u8::is_ascii_alphabetic).collect();
    if letters.len() < 5 {
        return false;
    }
    let vowels = letters.iter().filter(|c| matches!(c.to_ascii_lowercase(), b'a' | b'e' | b'i' | b'o' | b'u')).count();
    vowels as f64 / letters.len() as f64 >= 0.2
}

fn word_tokens(name: &str) -> usize {
    strip_extensions(name).split(['.', ' ', '\t', '\r', '\n', '_', '-']).filter(|t| m(&R.word3, t)).count()
}

fn strong_word_structure(name: &str, core: &str) -> bool {
    max_letters(core) >= 5 && normal_vowel_ratio(core) && word_tokens(name) >= 2
}

fn has_signal(name: &str) -> bool {
    [
        &R.signal_episode,
        &R.signal_season,
        &R.signal_resolution,
        &R.signal_codec,
        &R.signal_source,
        &R.signal_audio,
        &R.signal_tag,
        &R.year_word,
        &R.signal_group,
        &R.signal_ext,
    ]
    .iter()
    .any(|re| m(re, name))
}

fn zero_vowel_long(core: &str, min: usize) -> bool {
    if core.len() < min || !core.bytes().all(|b| b.is_ascii_alphanumeric()) || m(&R.year_word, core) {
        return false;
    }
    let letters: Vec<u8> = core.bytes().filter(|b| !b.is_ascii_digit()).collect();
    letters.len() >= 10 && !letters.iter().any(|c| matches!(c.to_ascii_lowercase(), b'a' | b'e' | b'i' | b'o' | b'u'))
}

fn generic_hash(name: &str) -> bool {
    let Some(c) = R.hash_generic.captures(name.as_bytes()) else { return false };
    let Some(token) = c.get(1).and_then(|t| std::str::from_utf8(t.as_bytes()).ok()) else { return false };
    !m(&R.year_word, token) && token.bytes().any(|b| b.is_ascii_digit()) && is_obfuscated(token)
}

/// R7. Whether the name has low signal, and what this pipe says of it.
fn misc_pipe(c: &Ctx<'_>) -> (bool, Option<Decision>) {
    let name = c.name;
    let core = core_name(&strip_extensions(name));
    let low_signal =
        !has_signal(name) && m(&R.core_token, &core) && core.len() >= 12 && !strong_word_structure(name, &core);

    let found = if is_obfuscated(name) {
        hit(HASHED, 900, "obfuscated_name")
    } else if m(&R.hash_md5, name) {
        hit(HASHED, 950, "hash_md5")
    } else if m(&R.hash_sha1, name) {
        hit(HASHED, 950, "hash_sha1")
    } else if m(&R.hash_sha256, name) {
        hit(HASHED, 950, "hash_sha256")
    } else if generic_hash(name) {
        hit(HASHED, 950, "hash_generic")
    } else if m(&R.punctuation, name) && !m(&R.punctuation_ext, name) && word_tokens(name) < 2 {
        // R11: the word count
        if low_signal || zero_vowel_long(&core, 12) {
            hit(HASHED, 750, "obfuscated_pattern")
        } else {
            hit(MISC, 500, "obfuscated_pattern")
        }
    } else if m(&R.archive, name) {
        hit(MISC, 500, "archive")
    } else if m(&R.dataset, name) && !m(&R.media_word, name) {
        hit(MISC, 600, "dataset")
    } else if m(&R.leak, name) && m(&R.dump, name) && !m(&R.media_word, name) {
        hit(MISC, 600, "data_dump")
    } else {
        None
    };
    (low_signal, found)
}

// ---------------------------------------------------------------- group

fn group_pipe(c: &Ctx<'_>) -> Option<Decision> {
    let g = c.group;
    if g.is_empty() {
        None
    } else if m(&R.g_anime, g) {
        hit(5070, 800, "group_name_anime")
    } else if m(&R.g_tv, g) {
        hit(5999, 600, "group_name_tv")
    } else if m(&R.g_movie, g) {
        hit(2999, 600, "group_name_movie")
    } else if m(&R.g_xxx, g) {
        hit(6999, 700, "group_name_xxx")
    } else if m(&R.g_music, g) {
        hit(3999, 600, "group_name_music")
    } else if m(&R.g_game, g) {
        hit(1999, 600, "group_name_game")
    } else if m(&R.g_pc, g) {
        hit(4010, 600, "group_name_pc")
    } else if m(&R.g_book, g) {
        hit(7020, 500, "group_name_book")
    } else {
        None
    }
}

// ---------------------------------------------------------------- xxx

const SEPARATOR_BYTES: &[u8] = b".-_ ";

/// R9: a studio at the start of a movie's name is the movie's title
fn looks_like_xxx(name: &str) -> bool {
    if m(&R.xxx_word, name) {
        return true;
    }
    if let Some(studio) = R.studios.find(name.as_bytes()) {
        let bytes = name.as_bytes();
        let at_start = studio.start() == 0 && bytes.get(studio.end()).is_some_and(|b| SEPARATOR_BYTES.contains(b));
        if at_start
            && R.movie_after_studio.is_match(&bytes[studio.end() + 1..])
            && m(&R.movie_source, name)
            && !m(&R.adult_keywords, name)
        {
            return false;
        }
        return true;
    }
    if m(&R.vr_sites, name) || m(&R.vr_generic, name) {
        return true;
    }
    if m(&R.vr_devices, name) && m(&R.vr_res, name) {
        return true;
    }
    if m(&R.adult_keywords, name) && m(&R.video_mark, name) {
        return true;
    }
    if m(&R.jav, name) && m(&R.studios, name) {
        return true;
    }
    if m(&R.site_date, name) {
        if m(&R.adult_keywords, name) {
            return true;
        }
        if m(&R.performer, name) && !m(&R.tv_not_adult, name) {
            return true;
        }
    }
    false
}

fn x_only_fans(c: &Ctx<'_>) -> Option<Decision> {
    let name = c.name.replace('_', " ");
    if m(&R.photo_pack, &name) && !m(&R.video_hint, &name) {
        return None;
    }
    if c.has_only_fans() { hit(6047, 950, "onlyfans") } else { None }
}

fn x_vr(name: &str) -> Option<Decision> {
    let known = m(&R.vr_sites, name);
    let generic = m(&R.vr_generic, name);
    let device = m(&R.vr_devices, name);
    let vr180 = m(&R.vr_180_360, name);
    if !vr180 && !known && !generic && !device {
        return None;
    }
    if !m(&R.vr_pattern, name) {
        return None;
    }
    if known {
        return hit(6046, 950, "vr_site");
    }
    if generic {
        return hit(6046, 900, "vr_site_generic");
    }
    if device && (m(&R.xxx_word, name) || m(&R.adult_keywords, name) || m(&R.studios, name)) {
        return hit(6046, 900, "vr_device");
    }
    if vr180 && (m(&R.xxx_word, name) || m(&R.adult_keywords, name) || m(&R.vr_sites, name)) {
        return hit(6046, 900, "vr_180_360");
    }
    None
}

fn x_uhd(name: &str) -> Option<Decision> {
    if !m(&R.x_uhd, name) {
        return None;
    }
    if !(m(&R.xxx_word, name) || m(&R.studios, name) || m(&R.x_uhd_marker, name)) {
        return None;
    }
    if m(&R.x_uhd_group, name) { hit(6045, 950, "uhd_group") } else { hit(6045, 900, "uhd") }
}

fn x_clip_hd(name: &str) -> Option<Decision> {
    if m(&R.x_pack_first, name) || m(&R.x_tv, name) {
        return None;
    }
    let has_hd = m(&R.x_hd, name);
    if m(&R.x_studio_title, name) {
        return hit(6041, 900, "clip_hd_studio");
    }
    if m(&R.x_studio_date, name) {
        return if has_hd { hit(6041, 950, "clip_hd_studio_date") } else { hit(6040, 850, "studio_date") };
    }
    if m(&R.x_date4, name) && !m(&R.x_not_docs, name) && (has_hd || m(&R.adult_keywords, name)) {
        return hit(6041, 850, "clip_hd_date_4digit");
    }
    if m(&R.x_date2, name) && !m(&R.x_not_docs, name) {
        return hit(6041, 850, "clip_hd_date");
    }
    if m(&R.x_jav_date, name) {
        return if has_hd { hit(6041, 900, "clip_hd_jav_date") } else { hit(6040, 850, "jav_date") };
    }
    if m(&R.x_studio_xxx, name) {
        return hit(6041, 900, "clip_hd_studio_xxx");
    }
    if m(&R.x_word_res, name) || m(&R.x_res_word, name) {
        return hit(6041, 800, "clip_hd_xxx");
    }
    None
}

fn x_web_dl(name: &str) -> Option<Decision> {
    if m(&R.x_ep, name) {
        return None;
    }
    if m(&R.web, name) && (m(&R.adult_keywords, name) || m(&R.studios, name) || m(&R.x_webdl_word, name)) {
        return hit(6090, 850, "webdl");
    }
    None
}

fn x_x264(name: &str) -> Option<Decision> {
    if m(&R.x_x265, name) || !m(&R.x_x264, name) || m(&R.x_not264, name) {
        return None;
    }
    if m(&R.x_adult264, name) { hit(6040, 850, "x264") } else { None }
}

fn x_wmv(name: &str) -> Option<Decision> {
    if m(&R.x_modern, name) {
        return None;
    }
    if m(&R.x_wmv, name) { hit(6020, 800, "wmv") } else { None }
}

fn xxx_pipe(c: &Ctx<'_>) -> Option<Decision> {
    let name = c.name;
    if !c.has_only_fans() && !looks_like_xxx(name) {
        return None;
    }
    x_only_fans(c)
        .or_else(|| x_vr(name))
        .or_else(|| x_uhd(name))
        .or_else(|| x_clip_hd(name))
        .or_else(|| if m(&R.x_pack, name) { hit(6050, 850, "pack") } else { None })
        .or_else(|| if m(&R.x_clip_poster, c.poster) { hit(6042, 850, "clip_sd_poster") } else { None })
        .or_else(
            || {
                if m(&R.x_clip_name, name) || contains_ci(name, "sdporn") { hit(6042, 850, "clip_sd") } else { None }
            },
        )
        .or_else(|| if m(&R.x_sd, name) { hit(6080, 850, "sd") } else { None })
        .or_else(|| if c.web_dl { x_web_dl(name) } else { None })
        .or_else(|| x_x264(name))
        .or_else(|| if m(&R.x_xvid, name) { hit(6030, 800, "xvid") } else { None })
        .or_else(|| if m(&R.x_imageset, name) { hit(6060, 900, "imageset") } else { None })
        .or_else(|| x_wmv(name))
        .or_else(|| if m(&R.x_dvd, name) { hit(6010, 850, "dvd") } else { None })
        .or_else(|| if m(&R.x_other, name) { hit(6999, 700, "other") } else { None })
}

// ---------------------------------------------------------------- tv

fn is_sport_event(name: &str) -> bool {
    m(&R.sports, name) && m(&R.sports_context, name)
}

fn looks_like_tv(c: &Ctx<'_>) -> bool {
    let n = c.name;
    c.standalone_season()
        || m(&R.tv_ep, n)
        || m(&R.tv_e, n)
        || m(&R.tv_complete, n)
        || m(&R.tv_pack, n)
        || m(&R.tv_episode_word, n)
        || m(&R.tv_source, n)
        || m(&R.tv_daily, n)
        || is_sport_event(n)
        || m(&R.anime_groups, n)
}

fn anime_strong(c: &Ctx<'_>) -> Option<Decision> {
    if !c.poster.is_empty() && m(&R.tosho, c.poster) {
        return hit(5070, 980, "anime_tosho_poster");
    }
    if !c.group.is_empty() && m(&R.g_anime, c.group) {
        return hit(5070, 900, "anime_group_name");
    }
    if m(&R.anime_groups, c.name) {
        return hit(5070, 920, "anime_group");
    }
    if m(&R.anime_hash, c.name) {
        return hit(5070, 900, "anime_hash");
    }
    None
}

fn anime(c: &Ctx<'_>) -> Option<Decision> {
    let n = c.name;
    if !c.poster.is_empty() && m(&R.tosho, c.poster) {
        return hit(5070, 980, "anime_tosho_poster");
    }
    if m(&R.anime_word, n) {
        return hit(5070, 950, "anime_pattern");
    }
    if m(&R.anime_groups, n) {
        return hit(5070, 950, "anime_group");
    }
    if m(&R.anime_hash, n) {
        return hit(5070, 900, "anime_hash");
    }
    if m(&R.anime_no, n) && (m(&R.anime_roman, n) || (m(&R.anime_e, n) && !m(&R.anime_se, n))) {
        return hit(5070, 900, "anime_japanese_title");
    }
    if m(&R.tv_e, n) && m(&R.anime_bd, n) && !m(&R.anime_s, n) {
        return hit(5070, 800, "anime_episode_bluray");
    }
    if m(&R.anime_roman_strict, n) && !m(&R.anime_se, n) {
        return hit(5070, 850, "anime_roman_numeral_season");
    }
    None
}

fn tv_uhd(n: &str) -> Option<Decision> {
    if m(&R.tv_se, n) && m(&R.res2160, n) {
        hit(5045, 900, "uhd_episode")
    } else if m(&R.tv_uhd_season, n) {
        hit(5045, 900, "uhd_season")
    } else if m(&R.tv_uhd_stream, n) {
        hit(5045, 900, "uhd_streaming")
    } else {
        None
    }
}

fn tv_pipe(c: &Ctx<'_>) -> Option<Decision> {
    if c.has_adult_markers() {
        return None;
    }
    let n = c.name;
    if let Some(found) = anime_strong(c) {
        return Some(found);
    }
    if !looks_like_tv(c) {
        return None;
    }
    anime(c)
        .or_else(|| if is_sport_event(n) { hit(5060, 850, "sport") } else { None })
        .or_else(|| if m(&R.tv_doc, n) { hit(5080, 850, "documentary") } else { None })
        .or_else(|| if c.foreign && m(&R.tv_foreign, n) { hit(5020, 800, "foreign_language") } else { None })
        .or_else(|| if m(&R.tv_x265, n) { hit(5090, 900, "x265_group") } else { None })
        .or_else(|| if c.web_dl && m(&R.web, n) { hit(5010, 850, "webdl") } else { None })
        .or_else(|| tv_uhd(n))
        .or_else(|| {
            if m(&R.tv_hd, n) {
                hit(5040, 850, "hd_resolution")
            } else if !c.web_dl && m(&R.web, n) {
                hit(5040, 800, "hd_webdl_fallback")
            } else {
                None
            }
        })
        .or_else(|| {
            if m(&R.tv_sd, n) {
                hit(5030, 800, "sd_format")
            } else if m(&R.tv_sd_codec, n) {
                hit(5030, 800, "sd_codec")
            } else {
                None
            }
        })
        .or_else(|| {
            if m(&R.tv_ep, n) {
                hit(5999, 600, "tv_other")
            } else if c.standalone_season() {
                hit(5999, 600, "tv_season_pack")
            } else {
                None
            }
        })
}

// ---------------------------------------------------------------- movie

fn movie_uhd(n: &str) -> Option<Decision> {
    if m(&R.m_uhd_tv, n) {
        return None;
    }
    if contains_ci(n, "2160p")
        || m(&R.m_uhd, n)
        || (m(&R.m_hdr, n) && m(&R.m_hevc, n))
        || (contains_ci(n, "uhd") && m(&R.m_br, n))
    {
        return hit(2045, 900, "uhd");
    }
    None
}

fn movie_foreign(n: &str) -> Option<Decision> {
    if m(&R.m_foreign, n) {
        hit(2010, 800, "foreign_language")
    } else if contains_ci(n, "castellano") {
        hit(2010, 800, "foreign_castellano")
    } else if m(&R.m_foreign2, n) {
        hit(2010, 850, "foreign_pattern")
    } else {
        None
    }
}

fn movie_pipe(c: &Ctx<'_>) -> Option<Decision> {
    let n = c.name;
    if c.has_adult_markers()
        || c.standalone_season()
        || m(&R.m_skip_tv, n)
        || m(&R.tv_e, n)
        || m(&R.m_skip_anime, n)
        || !m(&R.m_looks, n)
    {
        return None;
    }
    (if c.foreign { movie_foreign(n) } else { None })
        .or_else(|| if m(&R.m_x265, n) { hit(2090, 900, "x265_group") } else { None })
        .or_else(|| movie_uhd(n))
        .or_else(|| if m(&R.m_3d, n) { hit(2050, 900, "3d") } else { None })
        .or_else(|| if m(&R.m_bluray, n) && !m(&R.m_secret, n) { hit(2060, 900, "bluray") } else { None })
        .or_else(|| if m(&R.m_dvd, n) { hit(2070, 850, "dvd") } else { None })
        .or_else(|| if c.web_dl && m(&R.web, n) { hit(2080, 850, "webdl") } else { None })
        .or_else(|| {
            if m(&R.m_hd, n) {
                hit(2040, 850, "hd")
            } else if !c.web_dl && m(&R.web, n) {
                hit(2040, 800, "hd_webdl_fallback")
            } else {
                None
            }
        })
        .or_else(|| if m(&R.m_sd, n) { hit(2030, 800, "sd") } else { None })
        .or_else(|| if m(&R.m_cam, n) { hit(2999, 600, "cam") } else { None })
}

// ---------------------------------------------------------------- pc

fn pc_phone(n: &str) -> Option<Decision> {
    if m(&R.pc_ios, n) {
        hit(4060, 900, "ios")
    } else if m(&R.pc_android, n) {
        hit(4070, 900, "android")
    } else if m(&R.pc_phone, n) {
        hit(4040, 850, "phone_other")
    } else {
        None
    }
}

fn pc_game(c: &Ctx<'_>) -> Option<Decision> {
    let n = c.name;
    if m(&R.pc_console, n) || m(&R.pc_macos, n) || m(&R.pc_tv, n) {
        return None;
    }
    if m(&R.pc_group, n) || m(&R.pc_keyword, n) {
        return hit(4050, 900, "pc_game");
    }
    if m(&R.pc_poster, c.poster) {
        return hit(4050, 850, "pc_game_poster");
    }
    None
}

fn pc_0day(n: &str) -> Option<Decision> {
    if m(&R.pc_msix, n) {
        hit(4010, 900, "0day_msix_installer")
    } else if m(&R.pc_explicit, n) {
        hit(4010, 900, "0day_explicit")
    } else if m(&R.pc_system, n) {
        hit(4010, 850, "0day_system")
    } else if m(&R.pc_software, n) {
        hit(4010, 850, "0day_software")
    } else {
        None
    }
}

fn pc_pipe(c: &Ctx<'_>) -> Option<Decision> {
    let n = c.name;
    if c.has_adult_markers() || m(&R.pc_skip, n) {
        return None;
    }
    pc_phone(n)
        .or_else(|| {
            if m(&R.pc_mac, n) {
                hit(4030, 900, "mac")
            } else if m(&R.pc_macos, n) {
                hit(4030, 900, "macos")
            } else {
                None
            }
        })
        .or_else(|| pc_game(c))
        .or_else(|| {
            if m(&R.pc_iso, n) {
                hit(4020, 850, "iso")
            } else if m(&R.pc_training, n) {
                hit(4020, 900, "training_iso")
            } else {
                None
            }
        })
        .or_else(|| pc_0day(n))
}

// ---------------------------------------------------------------- console

fn console_pipe(c: &Ctx<'_>) -> Option<Decision> {
    let n = c.name;
    if c.has_adult_markers() || m(&R.pc_skip, n) {
        return None;
    }
    if m(&R.c_ps4_first, n) || m(&R.c_cusa, n) || m(&R.c_duplex, n) || m(&R.c_ps4, n) {
        hit(1180, 900, "ps4")
    } else if m(&R.c_ps3, n) {
        hit(1080, 900, "ps3")
    } else if m(&R.c_vita, n) {
        hit(1120, 900, "psvita")
    } else if m(&R.c_psp, n) {
        hit(1020, 900, "psp")
    } else if m(&R.c_xone, n) {
        hit(1140, 900, "xboxone")
    } else if m(&R.c_x360, n) {
        hit(1050, 900, "xbox360")
    } else if m(&R.c_xbox, n) && !m(&R.c_xbox_not, n) {
        hit(1040, 850, "xbox")
    } else if m(&R.c_wiiu, n) {
        hit(1130, 900, "wiiu")
    } else if m(&R.c_wii, n) && !m(&R.c_wiiu_word, n) {
        hit(1030, 850, "wii")
    } else if m(&R.c_3ds, n) {
        hit(1110, 900, "3ds")
    } else if m(&R.c_nds, n) {
        hit(1010, 900, "nds")
    } else if m(&R.c_retro, n) && m(&R.c_region, n) {
        hit(1999, 800, "retro_console")
    } else {
        None
    }
}

// ---------------------------------------------------------------- music

fn a_audiobook(n: &str) -> Option<Decision> {
    if m(&R.a_book, n) && (m(&R.a_book_word, n) || m(&R.a_book_len, n) || m(&R.a_book_ext, n)) {
        return hit(3030, 950, "audiobook");
    }
    if m(&R.a_book_tag, n) || m(&R.a_book_read, n) {
        return hit(3030, 900, "audiobook_pattern");
    }
    if m(&R.a_book_old, n) {
        return hit(3030, 850, "audiobook_legacy");
    }
    None
}

fn a_podcast(n: &str) -> Option<Decision> {
    if m(&R.a_pod, n) {
        hit(3050, 900, "podcast")
    } else if m(&R.a_pod_net, n) && m(&R.a_pod_ep, n) {
        hit(3050, 850, "podcast_network")
    } else if m(&R.a_pod_any, n) {
        hit(3050, 800, "podcast_simple")
    } else {
        None
    }
}

/// a foreign release is filed apart, at that rule's own confidence
fn or_foreign(
    c: &Ctx<'_>,
    foreign: (u16, &'static str),
    category: i64,
    confidence: u16,
    rule: &'static str,
) -> Option<Decision> {
    if c.foreign && m(&R.a_foreign, c.name) { hit(3060, foreign.0, foreign.1) } else { hit(category, confidence, rule) }
}

fn a_video(c: &Ctx<'_>) -> Option<Decision> {
    let n = c.name;
    if m(&R.a_mv, n) && (m(&R.a_mv_res, n) || m(&R.a_mv_word, n) || m(&R.a_mv_ext, n)) {
        return or_foreign(c, (850, "music_video_foreign"), 3020, 900, "music_video");
    }
    if m(&R.a_mv_artist, n) {
        return or_foreign(c, (800, "music_video_foreign"), 3020, 800, "music_video_artist");
    }
    None
}

fn a_lossless(c: &Ctx<'_>) -> Option<Decision> {
    let n = c.name;
    if m(&R.a_ll, n) && (m(&R.a_ll_word, n) || m(&R.a_ll_ext, n)) {
        return or_foreign(c, (900, "lossless_foreign"), 3040, 900, "lossless");
    }
    if m(&R.a_flac, n) || m(&R.a_flac2, n) {
        return or_foreign(c, (850, "flac_foreign"), 3040, 850, "flac");
    }
    if m(&R.a_ll_other, n) {
        return or_foreign(c, (850, "lossless_format_foreign"), 3040, 850, "lossless_format");
    }
    None
}

fn a_mp3(c: &Ctx<'_>) -> Option<Decision> {
    let n = c.name;
    if m(&R.a_mp3, n) && (m(&R.a_mp3_rate, n) || m(&R.a_mp3_src, n) || m(&R.a_mp3_more, n)) {
        return or_foreign(c, (850, "mp3_foreign"), 3010, 850, "mp3");
    }
    if m(&R.a_mp3_scene, n) {
        return or_foreign(c, (750, "mp3_scene_foreign"), 3010, 750, "mp3_scene");
    }
    if m(&R.a_mp3_bit, n) {
        return or_foreign(c, (800, "mp3_bitrate_foreign"), 3010, 800, "mp3_bitrate");
    }
    None
}

fn a_other(c: &Ctx<'_>) -> Option<Decision> {
    let n = c.name;
    if m(&R.a_other, n) {
        return or_foreign(c, (800, "music_other_foreign"), 3999, 800, "music_other");
    }
    if m(&R.a_album, n) || m(&R.a_album2, n) {
        return or_foreign(c, (750, "music_album_foreign"), 3999, 750, "music_album");
    }
    if m(&R.a_dj, n) || m(&R.a_dj2, n) {
        return or_foreign(c, (750, "music_dj_foreign"), 3999, 750, "music_dj");
    }
    None
}

fn music_pipe(c: &Ctx<'_>) -> Option<Decision> {
    let n = c.name;
    if c.has_adult_markers() || m(&R.pc_skip, n) {
        return None;
    }
    a_audiobook(n)
        .or_else(|| a_podcast(n))
        .or_else(|| a_video(c))
        .or_else(|| a_lossless(c))
        .or_else(|| a_mp3(c))
        .or_else(|| a_other(c))
}

// ---------------------------------------------------------------- book

fn software_markers(n: &str) -> bool {
    m(&R.b_soft, n) || m(&R.b_soft_vendor, n) || m(&R.b_soft_ext, n)
}

fn video_release(n: &str) -> bool {
    m(&R.b_res, n) && m(&R.b_video, n)
}

fn book_corroborator(n: &str) -> bool {
    m(&R.b_corroborate, n) || m(&R.b_corroborate_pub, n) || m(&R.b_corroborate_ext, n)
}

fn book_skip(c: &Ctx<'_>) -> bool {
    let n = c.name;
    c.has_adult_markers()
        || m(&R.b_ps4, n)
        || m(&R.b_console, n)
        || m(&R.b_game_id, n)
        || m(&R.b_soft_version, n)
        || software_markers(n)
        || m(&R.b_music, n)
        || m(&R.b_course, n)
        || video_release(n)
        || m(&R.b_distro, n)
        || m(&R.b_image, n)
        || m(&R.b_font, n)
        || m(&R.b_font_ext, n)
        || m(&R.pc_skip, n)
        || m(&R.b_movie, n)
}

fn b_comic(n: &str) -> Option<Decision> {
    if m(&R.b_comic, n) {
        hit(7030, 900, "comic_format")
    } else if m(&R.b_publisher, n) && m(&R.b_comic_word, n) {
        hit(7030, 850, "comic_publisher")
    } else if m(&R.b_manga, n) {
        hit(7030, 850, "manga")
    } else {
        None
    }
}

fn b_technical(n: &str) -> Option<Decision> {
    if software_markers(n) || m(&R.b_course, n) {
        return None;
    }
    if m(&R.b_tech_pub, n) {
        return hit(7040, 900, "technical_publisher");
    }
    if m(&R.b_subject, n) && m(&R.b_signal, n) && book_corroborator(n) {
        return hit(7040, 850, "technical_subject");
    }
    None
}

fn b_magazine(n: &str) -> Option<Decision> {
    let title = m(&R.b_mag_title, n);
    let issue = m(&R.b_issue, n);
    let date = m(&R.b_date, n);
    let frequency = m(&R.b_freq, n);
    let issue_title = m(&R.b_issue_title, n);
    let mcn = m(&R.b_mcn, n) && m(&R.b_magazine, n);
    if (frequency && (issue || date || title)) || (issue && (date || title || issue_title)) || mcn {
        return hit(7010, 900, "magazine_frequency");
    }
    if title {
        return hit(7010, 850, "magazine_title");
    }
    None
}

fn b_ebook(n: &str) -> Option<Decision> {
    if software_markers(n) {
        None
    } else if m(&R.b_strict_end, n) {
        hit(7020, 900, "ebook_format")
    } else if m(&R.b_strict, n) {
        hit(7020, 850, "ebook_indicator")
    } else if m(&R.b_pdf_end, n) && book_corroborator(n) {
        hit(7020, 750, "ebook_pdf_format")
    } else if m(&R.b_pdf, n) && book_corroborator(n) {
        hit(7020, 650, "ebook_pdf_indicator")
    } else if m(&R.b_platform, n) {
        hit(7020, 800, "ebook_platform")
    } else if book_corroborator(n) && m(&R.b_year, n) {
        hit(7020, 600, "ebook_book_signals")
    } else {
        None
    }
}

fn book_pipe(c: &Ctx<'_>) -> Option<Decision> {
    if book_skip(c) {
        return None;
    }
    let n = c.name;
    b_comic(n).or_else(|| b_technical(n)).or_else(|| b_magazine(n)).or_else(|| b_ebook(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(name: &'a str, group: &'a str) -> Input<'a> {
        Input { name, has_display_name: false, group, poster: "", is_obfuscated: false }
    }

    fn category(name: &str, group: &str) -> Decision {
        let full = format!("alt.binaries.{group}");
        classify(&input(name, &full), &Options::default())
    }

    fn misc(name: &str) -> Option<Decision> {
        let c = Ctx { name, group: "", poster: "", foreign: true, web_dl: false };
        misc_pipe(&c).1
    }

    #[test]
    fn an_obfuscated_or_unnamed_release_stays_hashed() {
        let cases = [
            ("", false, "unnamed"),
            ("   ", false, "unnamed"),
            ("Some.Show.S01E02.720p", true, "hidden"),
            ("abcdefghij0123456789", false, "hidden"),
            ("d41d8cd98f00b204e9800998ecf8427e", false, "hidden"),
        ];
        for (name, flag, rule) in cases {
            let i = Input { is_obfuscated: flag, ..input(name, "alt.binaries.teevee") };
            let d = classify(&i, &Options::default());
            assert_eq!((d.category, d.rule), (8020, rule), "{name:?}");
        }
    }

    #[test]
    fn misc_rules() {
        for name in [
            "Xq7.Zr2.Tk9.Wp4.Vn6.Ly8",
            "q7Zr2Tk9-Wp4Vn6Ly8.part01.rar",
            "kP93xQ7rL.ZMtwBfa",
            "123e4567-e89b-12d3-a456-426614174000",
            "aGVsbG8+d29ybGQ/Zm9vYmFyYmF6cXV4==",
        ] {
            let d = misc(name).unwrap();
            assert_eq!((d.category, d.rule), (8020, "obfuscated_name"), "{name}");
        }
        let d = misc("Some Name - d41d8cd98f00b204e9800998ecf8427e").unwrap();
        assert_eq!((d.category, d.rule), (8020, "hash_md5"));
        let d = misc("[abc] 7fG2kP9qLm3XwZ8vN4bR6tY1.rar").unwrap();
        assert_eq!((d.category, d.rule), (8020, "hash_generic"));
        let d = misc("2019-01-01_0001").unwrap();
        assert_eq!((d.category, d.rule), (8010, "obfuscated_pattern"));
        for name in ["files.part01.rar", "Some.Archive.Name.7z"] {
            let d = misc(name).unwrap();
            assert_eq!((d.category, d.rule), (8010, "archive"), "{name}");
        }
        let d = misc("backup_2019_full.sql").unwrap();
        assert_eq!((d.category, d.rule), (8010, "dataset"));
        for name in [
            "Plain.Name.Nothing.Else",
            "IMG_20190101_120000.jpg",
            "SOME.MOVIE.2019.1080P.BLURAY.X264-GRP",
            "Some_Show_S01E02_720p_HDTV_x264-GRP",
        ] {
            assert_eq!(misc(name), None, "{name}");
        }
    }

    #[test]
    fn stores_table() {
        let d = |rule: &'static str| Decision { category: 8010, confidence: 1, rule };
        for rule in ["no_match", "hidden", "unnamed", "obfuscated_name"] {
            assert!(!stores(&d(rule)), "{rule}");
        }
        for rule in ["hash_md5", "archive", "hd_resolution", "group_name_tv"] {
            assert!(stores(&d(rule)), "{rule}");
        }
    }

    pub(super) const TABLE: &[(&str, &str, i64)] = &[
        ("Some.Show.S01E02.720p.HDTV.x264-GRP", "teevee", 5040),
        ("Some.Show.S01E02.2160p.WEB.H265-GRP", "teevee", 5045),
        ("Some.Show.S01E02.WEB-DL.AAC2.0.H.264-GRP", "tv", 5040),
        ("Some.Show.S01E02.HDTV.XviD-GRP", "tv", 5030),
        ("Some.Show.S02.1080p.BluRay.x264-GRP", "tv", 5040),
        ("Some.Show.S02.COMPLETE.GERMAN.DUBBED.1080p.WEB-GRP", "tv", 5020),
        ("Some.Show.S03E04.German.720p.WEB.x264-GRP", "tv", 5020),
        ("Some.Docs.S01E01.Documentary.1080p.WEB.x264-GRP", "tv", 5080),
        ("[SubsPlease] Some Show - 08 (1080p) [ABCD1234].mkv", "anime", 5070),
        ("Some Show - 08 [1080p]", "multimedia.anime", 5070),
        ("NFL.2024.Week.03.Team.vs.Team.1080p.WEB.h264-GRP", "sports", 5060),
        ("Some.Daily.Show.2024.03.15.Guest.Name.720p.HDTV.x264-GRP", "teevee", 5040),
        ("Some.Show.S01E02.1080p.mkv", "misc", 5040),
        ("Teen.Drama.S01E02.1080p.WEB-DL.x264-GRP", "teevee", 5040),
        ("SOME.SHOW.S01E02.720P.HDTV.X264-GRP", "teevee", 5040),
        ("NFL.2024.WEEK.03.1080P.WEB.H264-GRP", "sports", 5060),
        ("Some_Show_S01E02_720p_HDTV_x264-GRP", "teevee", 5040),
        ("Some-Show-S01E02-720p-HDTV-x264-GRP", "teevee", 5040),
        ("Some.Movie.2019.1080p.BluRay.x264-GRP", "movies", 2040),
        ("Some.Movie.2019.2160p.UHD.BluRay.x265-GRP", "movies", 2045),
        ("Some.Movie.2019.COMPLETE.BLURAY-GRP", "bluray", 2060),
        ("Some.Movie.2019.BD50.Untouched-GRP", "bluray", 2060),
        ("Some.Movie.2019.3D.1080p.BluRay.SBS.x264-GRP", "movies", 2050),
        ("Some.Movie.2019.DVDRip.XviD-GRP", "movies", 2030),
        ("Some.Movie.2019.DVDR-GRP", "dvd", 2070),
        ("Some.Movie.2019.CAM.XviD-GRP", "movies", 2030),
        ("Some.Movie.2019.GERMAN.1080p.BluRay.x264-GRP", "movies", 2010),
        ("Some.Movie.2019.1080p.WEB-DL.DD5.1.H264-GRP", "movies", 2040),
        ("Wicked.2024.1080p.WEB-DL.DD5.1.x264-GRP", "movies", 2040),
        ("SOME.MOVIE.2019.1080P.BLURAY.X264-GRP", "movies", 2040),
        ("Artist-Album-2019-GRP", "mp3", 3010),
        ("Artist - Album (2019) [FLAC]", "sounds.flac", 3040),
        ("Artist-Album-WEB-2019-320kbps-MP3", "sounds.mp3", 3010),
        ("Artist-Album-WEB-320kbps-MP3", "sounds.mp3", 3010),
        ("Some.Author.Book.Name.Unabridged.Audiobook.M4B", "audiobooks", 3030),
        ("Some.Podcast.Episode.123", "sounds", 3050),
        ("Some Concert Live 2019 1080p BluRay x264-GRP", "music.videos", 3020),
        ("Some Game-CODEX", "games", 4050),
        ("Some.Game.Update.1-FLT", "games", 4050),
        ("Some.App.v1.2.3.Incl.Keygen-GRP", "warez", 4010),
        ("Some.App.v2.MacOSX-GRP", "mac", 4030),
        ("Some.Tool.Android-GRP", "warez", 4070),
        ("Some.Game.PS4-DUPLEX", "games", 1180),
        ("Some.Game.XBOX360-GRP", "games.xbox360", 1050),
        ("Some.Game.USA.NSW-GRP", "games", 1999),
        ("Some.Book.Title.2020.RETAIL.EPUB-GRP", "ebook", 7020),
        ("Some Book Title (2020).epub", "e-book", 7020),
        ("Some Comic 012 (2020) (Digital).cbr", "comics", 7030),
        ("Learning Rust, 2nd Edition - OReilly.pdf", "e-book", 7040),
        ("Forbes.USA.March.2024.PDF", "magazines", 7010),
        ("Some.Title.XXX.1080p.WEB.x264-GRP", "erotica", 6041),
        ("Some.Title.XXX.2160p.MP4-GRP", "erotica", 6045),
        ("Some.Title.XXX.DVDRip.x264-GRP", "erotica", 6040),
        ("Some.Title.XXX.IMAGESET-GRP", "erotica", 6060),
        ("Some.Title.XXX.IMAGESET.jpg", "erotica", 6060),
        ("photo_001.jpg", "pictures.erotica", 6999),
        ("Some.Movie.2019.1080p.jpg", "movies", 8000),
        ("cover.jpg", "e-book", 8000),
        ("cover.jpg", "anime", 8000),
        ("IMG_20190101_120000.jpg", "pictures", 8000),
        ("holiday_photos_2019", "pictures", 8000),
        ("Plain Name Nothing Else", "misc", 8000),
        ("Plain Name Nothing Else", "teevee", 5999),
        ("Plain Name Nothing Else", "movies", 2999),
        ("Plain Name Nothing Else", "mp3", 3999),
        ("Plain Name Nothing Else", "warez", 4010),
        ("Plain Name Nothing Else", "e-book", 7020),
        ("Kw3Rt7Yp9Zs2", "teevee", 8020),
        ("mountainlake", "teevee", 8010),
    ];

    #[test]
    fn the_rules_table() {
        let mut wrong = Vec::new();
        for (name, group, want) in TABLE {
            let d = category(name, group);
            if d.category != *want {
                wrong.push(format!("{name} in {group}: got {} by {}, want {want}", d.category, d.rule));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn a_display_name_clears_hidden() {
        let i = Input {
            has_display_name: true,
            is_obfuscated: true,
            ..input("Some.Show.S01E02.720p", "alt.binaries.teevee")
        };
        let d = classify(&i, &Options::default());
        assert_eq!((d.category, d.rule), (5040, "hd_resolution"));
    }

    #[test]
    fn options_change_the_sub_not_the_root() {
        let with = |name: &str, group: &str, o: Options| {
            let full = format!("alt.binaries.{group}");
            classify(&input(name, &full), &o).category
        };
        let web = Options { foreign: true, web_dl: true };
        assert_eq!(with("Some.Show.S01E02.WEB-DL.AAC2.0.H.264-GRP", "tv", web), 5010);
        assert_eq!(with("Some.Movie.2019.1080p.WEB-DL.DD5.1.H264-GRP", "movies", web), 2080);
        let no_foreign = Options { foreign: false, web_dl: false };
        assert_eq!(with("Some.Show.S03E04.German.720p.WEB.x264-GRP", "tv", no_foreign), 5040);
        for (name, group, want) in TABLE {
            for foreign in [true, false] {
                for web_dl in [true, false] {
                    let got = with(name, group, Options { foreign, web_dl });
                    assert_eq!(super::super::table::root_of(got), super::super::table::root_of(*want), "{name}");
                }
            }
        }
    }

    #[test]
    fn a_poster_can_decide() {
        let i = Input { poster: "anon <x@animetosho.org>", ..input("Some.Show.08.mkv", "alt.binaries.misc") };
        assert_eq!(classify(&i, &Options::default()).category, 5070);
    }

    #[test]
    fn a_group_hint_loses_to_a_name_and_never_beats_a_hash() {
        assert_eq!(category("Some.Movie.2019.1080p.BluRay.x264-GRP", "teevee").category, 2040);
        assert_eq!(category("d41d8cd98f00b204e9800998ecf8427e", "teevee").category, 8020);
    }

    #[test]
    fn only_leaves_are_stored() {
        for (name, group, _) in TABLE {
            let d = category(name, group);
            if stores(&d) {
                assert!(super::super::table::is_leaf(d.category), "{name}: {}", d.category);
            }
        }
    }

    #[test]
    #[ignore]
    fn classify_is_cheap() {
        let start = std::time::Instant::now();
        let mut calls = 0u32;
        while calls < 200_000 {
            for (name, group, _) in TABLE {
                std::hint::black_box(category(name, group));
                calls += 1;
            }
        }
        println!("{:.2} us a call", start.elapsed().as_secs_f64() * 1e6 / f64::from(calls));
    }
}
