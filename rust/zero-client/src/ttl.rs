//! Query time-to-live (`packages/zql/src/query/ttl.ts`).

use crate::json::{Value, number};

/// How long a query stays cached after its last use.
///
/// `Forever` and negative durations never expire, `None` expires
/// immediately. Durations are clamped to 10 minutes when sent to the server.
#[derive(Clone, Debug, PartialEq)]
pub enum Ttl {
    /// Expires as soon as nothing uses the query.
    None,
    /// Never expires.
    Forever,
    /// A duration in milliseconds.
    Milliseconds(f64),
    /// A duration string as JS accepts it: a number followed by `s`, `m`,
    /// `h`, `d`, or `y` (for example `"5m"`).
    String(String),
}

/// The TTL a query gets when it doesn't name one: five minutes.
pub const DEFAULT_TTL_MILLISECONDS: f64 = 5.0 * 60.0 * 1000.0;
/// The longest TTL the server keeps a query for: ten minutes.
pub const MAX_TTL_MILLISECONDS: f64 = 10.0 * 60.0 * 1000.0;

fn multiplier(unit: char) -> Option<f64> {
    Some(match unit {
        's' => 1000.0,
        'm' => 60.0 * 1000.0,
        'h' => 60.0 * 60.0 * 1000.0,
        'd' => 24.0 * 60.0 * 60.0 * 1000.0,
        'y' => 365.0 * 24.0 * 60.0 * 60.0 * 1000.0,
        _ => return None,
    })
}

impl Ttl {
    /// The default TTL as a string.
    pub const DEFAULT: &str = "5m";

    /// `n` seconds.
    pub fn seconds(n: f64) -> Ttl {
        Ttl::String(number::format(n) + "s")
    }

    /// `n` minutes.
    pub fn minutes(n: f64) -> Ttl {
        Ttl::String(number::format(n) + "m")
    }

    /// `n` hours.
    pub fn hours(n: f64) -> Ttl {
        Ttl::String(number::format(n) + "h")
    }

    /// `n` days.
    pub fn days(n: f64) -> Ttl {
        Ttl::String(number::format(n) + "d")
    }

    /// A TTL as JS holds it: a string or a number.
    pub fn from_json(v: &Value) -> Option<Ttl> {
        match v {
            Value::Number(n) => Some(Ttl::Milliseconds(*n)),
            Value::String(s) => Some(Ttl::from(s.as_str())),
            _ => None,
        }
    }

    /// The TTL as JS holds it.
    pub fn to_json(&self) -> Value {
        match self {
            Ttl::None => "none".into(),
            Ttl::Forever => "forever".into(),
            Ttl::Milliseconds(n) => Value::Number(*n),
            Ttl::String(s) => s.as_str().into(),
        }
    }

    /// `parseTTL`: milliseconds, with `-1` for never expiring.
    pub fn parsed(&self) -> f64 {
        match self {
            Ttl::None => 0.0,
            Ttl::Forever => -1.0,
            Ttl::Milliseconds(n) if n.is_nan() => 0.0,
            Ttl::Milliseconds(n) if !n.is_finite() || *n < 0.0 => -1.0,
            Ttl::Milliseconds(n) => *n,
            Ttl::String(s) if s == "none" => 0.0,
            Ttl::String(s) if s == "forever" => -1.0,
            Ttl::String(s) => {
                let Some(unit) = s.chars().last() else {
                    return f64::NAN;
                };
                match multiplier(unit) {
                    Some(m) => number::parse(&s[..s.len() - unit.len_utf8()]) * m,
                    None => f64::NAN,
                }
            }
        }
    }

    /// `compareTTL`: never-expiring sorts last.
    pub fn compare(a: &Ttl, b: &Ttl) -> f64 {
        let (ap, bp) = (a.parsed(), b.parsed());
        match (ap == -1.0, bp == -1.0) {
            (true, false) => 1.0,
            (false, true) => -1.0,
            _ => ap - bp,
        }
    }

    /// `normalizeTTL`: the shortest equivalent spelling of a number of
    /// milliseconds; strings are returned as is.
    pub fn normalized(&self) -> Ttl {
        let Ttl::Milliseconds(ttl) = *self else {
            return self.clone();
        };
        if ttl < 0.0 {
            return Ttl::Forever;
        }
        if ttl == 0.0 {
            return Ttl::None;
        }
        let plain = number::format(ttl);
        let mut shortest = plain.clone();
        for unit in ['y', 'd', 'h', 'm', 's'] {
            let candidate =
                number::format(ttl / multiplier(unit).expect("unit")) + &unit.to_string();
            if candidate.encode_utf16().count() < shortest.encode_utf16().count() {
                shortest = candidate;
            }
        }
        if shortest.len() < plain.len() {
            Ttl::String(shortest)
        } else {
            self.clone()
        }
    }

    /// `clampTTL`: milliseconds, at most 10 minutes; never-expiring becomes
    /// 10 minutes.
    pub fn clamped(&self) -> f64 {
        let p = self.parsed();
        if p == -1.0 || p > MAX_TTL_MILLISECONDS {
            MAX_TTL_MILLISECONDS
        } else {
            p
        }
    }
}

impl From<&str> for Ttl {
    fn from(s: &str) -> Ttl {
        match s {
            "none" => Ttl::None,
            "forever" => Ttl::Forever,
            s => Ttl::String(s.to_owned()),
        }
    }
}

impl Default for Ttl {
    fn default() -> Ttl {
        Ttl::String(Ttl::DEFAULT.to_owned())
    }
}
