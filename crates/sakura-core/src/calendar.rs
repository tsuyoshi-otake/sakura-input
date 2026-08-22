//! Civil-date conversion surfaces for readings such as `きょう`.
//!
//! The converter stays a pure function of `(dictionary, reading, civil date)`.
//! Local time belongs to the engine; this module only formats a supplied day.

use sakura_proto::Overflow;

use crate::TextSink;

const DATE_READINGS: [(&str, i32); 7] = [
    ("きょう", 0),
    ("こんにち", 0),
    ("ほんじつ", 0),
    ("あさって", 2),
    ("みょうごにち", 2),
    ("らいしゅう", 7),
    ("せんしゅう", -7),
];

/// Gregorian calendar day in the civil (year, month, day) form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CivilDate {
    year: i32,
    month: u8,
    day: u8,
}

/// Weekday with Sunday as the first variant, matching the Gregorian cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weekday {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

/// Japanese era year used for 和暦 surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JapaneseEraYear {
    name: &'static str,
    year: u16,
}

/// One generated date surface offered beside the ordinary lexical candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateSurfaceSpec {
    pub format: DateFormat,
    pub annotation: &'static str,
}

/// Bounded set of date spellings for one civil day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateFormat {
    JapaneseEra,
    JapaneseEraWeekday,
    Gregorian,
    GregorianWeekday,
}

const DATE_SURFACE_SPECS: [DateSurfaceSpec; 4] = [
    DateSurfaceSpec {
        format: DateFormat::JapaneseEra,
        annotation: "和暦",
    },
    DateSurfaceSpec {
        format: DateFormat::JapaneseEraWeekday,
        annotation: "和暦・曜日",
    },
    DateSurfaceSpec {
        format: DateFormat::Gregorian,
        annotation: "西暦",
    },
    DateSurfaceSpec {
        format: DateFormat::GregorianWeekday,
        annotation: "西暦・曜日",
    },
];

const ERA_TABLE: [(&str, CivilDate); 5] = [
    (
        "令和",
        CivilDate {
            year: 2019,
            month: 5,
            day: 1,
        },
    ),
    (
        "平成",
        CivilDate {
            year: 1989,
            month: 1,
            day: 8,
        },
    ),
    (
        "昭和",
        CivilDate {
            year: 1926,
            month: 12,
            day: 25,
        },
    ),
    (
        "大正",
        CivilDate {
            year: 1912,
            month: 7,
            day: 30,
        },
    ),
    (
        "明治",
        CivilDate {
            year: 1868,
            month: 1,
            day: 25,
        },
    ),
];

const WEEKDAY_KANJI: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];
const DAYS_IN_MONTH: [u8; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const SAKAMOTO: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];

impl CivilDate {
    /// Accepts a real Gregorian civil date in `1..=9999`.
    pub fn from_ymd(year: i32, month: u8, day: u8) -> Option<Self> {
        if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
            return None;
        }
        let max_day = days_in_month(year, month)?;
        if day == 0 || day > max_day {
            return None;
        }
        Some(Self { year, month, day })
    }

    pub const fn year(self) -> i32 {
        self.year
    }

    pub const fn month(self) -> u8 {
        self.month
    }

    pub const fn day(self) -> u8 {
        self.day
    }

    /// Shifts this civil day by `days`, crossing month and year boundaries.
    /// Dates outside `1..=9999` fail closed.
    pub fn add_days(self, days: i32) -> Option<Self> {
        match days.cmp(&0) {
            core::cmp::Ordering::Equal => Some(self),
            core::cmp::Ordering::Greater => {
                let mut date = self;
                for _ in 0..days {
                    date = date.successor()?;
                }
                Some(date)
            }
            core::cmp::Ordering::Less => {
                let mut date = self;
                for _ in 0..days.unsigned_abs() {
                    date = date.predecessor()?;
                }
                Some(date)
            }
        }
    }

    fn successor(self) -> Option<Self> {
        let next_day = self.day.checked_add(1)?;
        if next_day <= days_in_month(self.year, self.month)? {
            return Self::from_ymd(self.year, self.month, next_day);
        }
        if self.month < 12 {
            return Self::from_ymd(self.year, self.month + 1, 1);
        }
        Self::from_ymd(self.year.checked_add(1)?, 1, 1)
    }

    fn predecessor(self) -> Option<Self> {
        if self.day > 1 {
            return Self::from_ymd(self.year, self.month, self.day - 1);
        }
        if self.month > 1 {
            let month = self.month - 1;
            return Self::from_ymd(self.year, month, days_in_month(self.year, month)?);
        }
        let year = self.year.checked_sub(1)?;
        Self::from_ymd(year, 12, 31)
    }

    pub fn weekday(self) -> Weekday {
        let mut year = self.year;
        if self.month < 3 {
            year -= 1;
        }
        let index = year + year / 4 - year / 100
            + year / 400
            + SAKAMOTO[usize::from(self.month) - 1]
            + i32::from(self.day);
        match index.rem_euclid(7) {
            0 => Weekday::Sunday,
            1 => Weekday::Monday,
            2 => Weekday::Tuesday,
            3 => Weekday::Wednesday,
            4 => Weekday::Thursday,
            5 => Weekday::Friday,
            _ => Weekday::Saturday,
        }
    }

    pub fn japanese_era(self) -> Option<JapaneseEraYear> {
        for (name, start) in ERA_TABLE {
            if self.cmp_ymd(start) >= 0 {
                let year = u16::try_from(self.year.saturating_sub(start.year).saturating_add(1))
                    .ok()
                    .filter(|year| *year > 0)?;
                return Some(JapaneseEraYear { name, year });
            }
        }
        None
    }

    fn cmp_ymd(self, other: Self) -> i32 {
        if self.year != other.year {
            return self.year - other.year;
        }
        if self.month != other.month {
            return i32::from(self.month) - i32::from(other.month);
        }
        i32::from(self.day) - i32::from(other.day)
    }
}

impl JapaneseEraYear {
    pub const fn name(self) -> &'static str {
        self.name
    }

    pub const fn year(self) -> u16 {
        self.year
    }
}

impl Weekday {
    pub const fn kanji(self) -> &'static str {
        WEEKDAY_KANJI[self as usize]
    }
}

/// Day offset from today for an exact whole-query date reading.
pub fn date_offset_for_reading(reading: &str) -> Option<i32> {
    DATE_READINGS
        .iter()
        .find_map(|(candidate, offset)| (*candidate == reading).then_some(*offset))
}

/// Exact whole-query readings that may grow date surfaces.
pub fn is_today_date_reading(reading: &str) -> bool {
    date_offset_for_reading(reading).is_some()
}

/// Specs in display order: 和暦, 和暦+曜日, 西暦, 西暦+曜日.
pub fn date_surface_specs(date: CivilDate) -> impl Iterator<Item = DateSurfaceSpec> {
    DATE_SURFACE_SPECS
        .into_iter()
        .filter(move |spec| spec.format.supported_for(date))
}

impl DateFormat {
    fn supported_for(self, date: CivilDate) -> bool {
        !matches!(self, Self::JapaneseEra | Self::JapaneseEraWeekday)
            || date.japanese_era().is_some()
    }

    pub fn write(self, date: CivilDate, sink: &mut impl TextSink) -> Result<(), Overflow> {
        match self {
            Self::JapaneseEra => write_japanese(date, false, sink),
            Self::JapaneseEraWeekday => write_japanese(date, true, sink),
            Self::Gregorian => write_gregorian(date, false, sink),
            Self::GregorianWeekday => write_gregorian(date, true, sink),
        }
    }
}

fn days_in_month(year: i32, month: u8) -> Option<u8> {
    let days = *DAYS_IN_MONTH.get(usize::from(month).checked_sub(1)?)?;
    if month == 2 && is_leap_year(year) {
        Some(29)
    } else {
        Some(days)
    }
}

fn is_leap_year(year: i32) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

fn write_japanese(
    date: CivilDate,
    weekday: bool,
    sink: &mut impl TextSink,
) -> Result<(), Overflow> {
    let era = date.japanese_era().ok_or(Overflow)?;
    sink.push_str(era.name)?;
    if era.year == 1 {
        sink.push_str("元")?;
    } else {
        write_u32(sink, u32::from(era.year))?;
    }
    sink.push_str("年")?;
    write_u32(sink, u32::from(date.month))?;
    sink.push_str("月")?;
    write_u32(sink, u32::from(date.day))?;
    sink.push_str("日")?;
    write_weekday_suffix(date, weekday, sink)
}

fn write_gregorian(
    date: CivilDate,
    weekday: bool,
    sink: &mut impl TextSink,
) -> Result<(), Overflow> {
    write_u32(sink, u32::try_from(date.year).map_err(|_| Overflow)?)?;
    sink.push_str("年")?;
    write_u32(sink, u32::from(date.month))?;
    sink.push_str("月")?;
    write_u32(sink, u32::from(date.day))?;
    sink.push_str("日")?;
    write_weekday_suffix(date, weekday, sink)
}

fn write_weekday_suffix(
    date: CivilDate,
    weekday: bool,
    sink: &mut impl TextSink,
) -> Result<(), Overflow> {
    if !weekday {
        return Ok(());
    }
    sink.push_str("（")?;
    sink.push_str(date.weekday().kanji())?;
    sink.push_str("）")
}

fn write_u32(sink: &mut impl TextSink, value: u32) -> Result<(), Overflow> {
    write_u32_width(sink, value, 1)
}

fn write_u32_width(sink: &mut impl TextSink, mut value: u32, width: usize) -> Result<(), Overflow> {
    let mut digits = [b'0'; 10];
    let mut written = 0usize;
    loop {
        written += 1;
        digits[digits.len() - written] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let len = written.max(width.min(digits.len()));
    let text = core::str::from_utf8(&digits[digits.len() - len..]).unwrap_or("");
    sink.push_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ymd(year: i32, month: u8, day: u8) -> CivilDate {
        CivilDate::from_ymd(year, month, day).expect("valid civil date")
    }

    fn render(date: CivilDate, format: DateFormat) -> String {
        let mut text = String::new();
        format.write(date, &mut text).expect("date fits");
        text
    }

    #[test]
    fn rejects_impossible_civil_dates() {
        assert_eq!(CivilDate::from_ymd(2021, 2, 29), None);
        assert_eq!(CivilDate::from_ymd(2026, 13, 1), None);
        assert_eq!(CivilDate::from_ymd(2026, 0, 1), None);
        assert_eq!(CivilDate::from_ymd(0, 1, 1), None);
        assert_eq!(CivilDate::from_ymd(2026, 4, 31), None);
        assert!(CivilDate::from_ymd(2020, 2, 29).is_some());
        assert!(CivilDate::from_ymd(2000, 2, 29).is_some());
        assert_eq!(CivilDate::from_ymd(1900, 2, 29), None);
    }

    #[test]
    fn weekday_matches_known_gregorian_days() {
        assert_eq!(ymd(1970, 1, 1).weekday(), Weekday::Thursday);
        assert_eq!(ymd(2000, 1, 1).weekday(), Weekday::Saturday);
        assert_eq!(ymd(2019, 5, 1).weekday(), Weekday::Wednesday);
        assert_eq!(ymd(2026, 8, 19).weekday(), Weekday::Wednesday);
        assert_eq!(ymd(2020, 2, 29).weekday(), Weekday::Saturday);
    }

    #[test]
    fn reiwa_and_heisei_boundaries() {
        let reiwa_start = ymd(2019, 5, 1).japanese_era().expect("Reiwa");
        assert_eq!(reiwa_start.name(), "令和");
        assert_eq!(reiwa_start.year(), 1);
        let heisei_end = ymd(2019, 4, 30).japanese_era().expect("Heisei");
        assert_eq!(heisei_end.name(), "平成");
        assert_eq!(heisei_end.year(), 31);
        let today = ymd(2026, 8, 19).japanese_era().expect("Reiwa 8");
        assert_eq!(today.name(), "令和");
        assert_eq!(today.year(), 8);
        assert!(ymd(1868, 1, 24).japanese_era().is_none());
    }

    #[test]
    fn today_surfaces_cover_era_gregorian_and_weekday_variants() {
        let date = ymd(2026, 8, 19);
        let surfaces: Vec<String> = date_surface_specs(date)
            .map(|spec| render(date, spec.format))
            .collect();
        assert_eq!(
            surfaces,
            [
                "令和8年8月19日",
                "令和8年8月19日（水）",
                "2026年8月19日",
                "2026年8月19日（水）",
            ]
        );
    }

    #[test]
    fn first_era_year_uses_gannen() {
        assert_eq!(
            render(ymd(2019, 5, 1), DateFormat::JapaneseEra),
            "令和元年5月1日"
        );
    }

    #[test]
    fn exact_relative_date_readings_are_the_only_triggers() {
        assert_eq!(date_offset_for_reading("きょう"), Some(0));
        assert_eq!(date_offset_for_reading("こんにち"), Some(0));
        assert_eq!(date_offset_for_reading("ほんじつ"), Some(0));
        assert_eq!(date_offset_for_reading("あさって"), Some(2));
        assert_eq!(date_offset_for_reading("みょうごにち"), Some(2));
        assert_eq!(date_offset_for_reading("らいしゅう"), Some(7));
        assert_eq!(date_offset_for_reading("せんしゅう"), Some(-7));
        assert!(!is_today_date_reading("きょうは"));
        assert!(!is_today_date_reading("らいしゅうは"));
        assert!(!is_today_date_reading("きのう"));
        assert!(!is_today_date_reading("今日"));
        assert!(!is_today_date_reading("来週"));
    }

    #[test]
    fn add_days_crosses_month_and_year_boundaries() {
        assert_eq!(ymd(2026, 8, 19).add_days(0), Some(ymd(2026, 8, 19)));
        assert_eq!(ymd(2026, 8, 19).add_days(2), Some(ymd(2026, 8, 21)));
        assert_eq!(ymd(2026, 8, 19).add_days(7), Some(ymd(2026, 8, 26)));
        assert_eq!(ymd(2026, 8, 19).add_days(-7), Some(ymd(2026, 8, 12)));
        assert_eq!(ymd(2026, 8, 30).add_days(2), Some(ymd(2026, 9, 1)));
        assert_eq!(ymd(2026, 1, 3).add_days(-7), Some(ymd(2025, 12, 27)));
        assert_eq!(ymd(2020, 2, 28).add_days(2), Some(ymd(2020, 3, 1)));
        assert_eq!(ymd(2021, 2, 28).add_days(1), Some(ymd(2021, 3, 1)));
        assert_eq!(ymd(1, 1, 1).add_days(-1), None);
        assert_eq!(ymd(9999, 12, 31).add_days(1), None);
        assert_eq!(ymd(2026, 8, 26).weekday(), Weekday::Wednesday);
        assert_eq!(ymd(2026, 8, 21).weekday(), Weekday::Friday);
        assert_eq!(ymd(2026, 8, 12).weekday(), Weekday::Wednesday);
    }
}
