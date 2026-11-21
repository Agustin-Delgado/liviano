//! What the bookmarks, history and downloads panels share: the search rule
//! and the way a row reads. Pure functions, tested without a window.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelKind {
    Bookmarks,
    History,
    Downloads,
    Passwords,
}

impl PanelKind {
    pub fn cue(self) -> &'static str {
        match self {
            PanelKind::Bookmarks => "Marcadores: buscá por nombre o dirección · Supr borra",
            PanelKind::History => "Historial: buscá por nombre o dirección · Supr borra",
            PanelKind::Downloads => "Descargas: Enter abre el archivo · Shift+Enter, la carpeta · Supr quita",
            PanelKind::Passwords => "Contraseñas: Enter copia · Shift+Enter muestra · Supr borra",
        }
    }
}

/// Lower case without the Spanish accents, so "cancion" finds "Canción".
pub fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

/// Every word of the query appears in one of the fields (an empty query
/// matches everything).
pub fn matches(query: &str, fields: &[&str]) -> bool {
    Query::new(query).matches(fields)
}

/// A search folded once, to test against many records: the history panel
/// runs it over tens of thousands of entries per keystroke.
pub struct Query(Vec<String>);

impl Query {
    pub fn new(query: &str) -> Self {
        Query(query.split_whitespace().map(fold).collect())
    }

    pub fn matches(&self, fields: &[&str]) -> bool {
        self.0.iter().all(|word| fields.iter().any(|field| contains_folded(field, word)))
    }
}

/// `field` contains `word` (already folded). An ASCII field, nearly every
/// address and most titles, is compared in place instead of folded first.
fn contains_folded(field: &str, word: &str) -> bool {
    if !field.is_ascii() {
        return fold(field).contains(word);
    }
    let (field, word) = (field.as_bytes(), word.as_bytes());
    word.is_empty() || field.windows(word.len()).any(|w| w.eq_ignore_ascii_case(word))
}

/// "3,2 MB", with the decimal comma the rest of the screen uses.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let text = if value >= 100.0 { format!("{value:.0}") } else { format!("{value:.1}") };
    format!("{} {}", text.replace('.', ","), UNITS[unit])
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// When something happened, as a history row says it: "hoy 14:32",
/// "ayer 09:05", "12/11/2026". `offset` is the local time zone in seconds.
pub fn format_when(at: u64, now: u64, offset: i64) -> String {
    let local = at as i64 + offset;
    let day = local.div_euclid(86_400);
    let today = (now as i64 + offset).div_euclid(86_400);
    let minutes = local.rem_euclid(86_400) / 60;
    let clock = format!("{:02}:{:02}", minutes / 60, minutes % 60);
    match today - day {
        0 => format!("hoy {clock}"),
        1 => format!("ayer {clock}"),
        _ => {
            let (y, m, d) = civil_from_days(day);
            format!("{d:02}/{m:02}/{y}")
        }
    }
}
