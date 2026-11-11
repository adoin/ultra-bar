use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::SYSTEMTIME;

pub const DEFAULT_FORMAT: &str = "MM-DD HH:mm";
pub fn format(pattern: &str, time: &SYSTEMTIME) -> Result<String> {
    if pattern.is_empty() || pattern.chars().count() > 96 {
        bail!("时间格式需为 1–96 个字符");
    }
    let mut rest = pattern;
    let mut output = String::new();
    while !rest.is_empty() {
        if rest.starts_with('[') {
            let Some(end) = rest.find(']') else {
                bail!("固定文字的方括号未闭合");
            };
            output.push_str(&rest[1..end]);
            rest = &rest[end + 1..];
            continue;
        }
        let tokens = [
            "YYYY", "dddd", "ddd", "YY", "MM", "DD", "HH", "hh", "mm", "ss", "M", "D", "H", "h",
            "m", "s", "A",
        ];
        if let Some(token) = tokens.into_iter().find(|token| rest.starts_with(token)) {
            let hour12 = match time.wHour % 12 {
                0 => 12,
                hour => hour,
            };
            let value = match token {
                "YYYY" => format!("{:04}", time.wYear),
                "YY" => format!("{:02}", time.wYear % 100),
                "MM" => format!("{:02}", time.wMonth),
                "M" => time.wMonth.to_string(),
                "DD" => format!("{:02}", time.wDay),
                "D" => time.wDay.to_string(),
                "HH" => format!("{:02}", time.wHour),
                "H" => time.wHour.to_string(),
                "hh" => format!("{hour12:02}"),
                "h" => hour12.to_string(),
                "mm" => format!("{:02}", time.wMinute),
                "m" => time.wMinute.to_string(),
                "ss" => format!("{:02}", time.wSecond),
                "s" => time.wSecond.to_string(),
                "A" => if time.wHour < 12 { "AM" } else { "PM" }.into(),
                "ddd" => format!(
                    "周{}",
                    ["日", "一", "二", "三", "四", "五", "六"][time.wDayOfWeek as usize % 7]
                ),
                "dddd" => format!(
                    "星期{}",
                    ["日", "一", "二", "三", "四", "五", "六"][time.wDayOfWeek as usize % 7]
                ),
                _ => unreachable!(),
            };
            output.push_str(&value);
            rest = &rest[token.len()..];
        } else {
            let character = rest.chars().next().unwrap();
            if character.is_ascii_alphabetic() || character == ']' {
                bail!("不支持的时间标记：{character}；固定文字请放在 [] 内");
            }
            if character.is_control() {
                bail!("时间格式不能包含换行或控制字符");
            }
            output.push(character);
            rest = &rest[character.len_utf8()..];
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats_month_minutes_seconds_and_literals() {
        let time = SYSTEMTIME {
            wYear: 2026,
            wMonth: 11,
            wDay: 10,
            wHour: 13,
            wMinute: 8,
            wSecond: 7,
            wDayOfWeek: 2,
            ..Default::default()
        };
        assert_eq!(format("MM-DD HH:mm", &time).unwrap(), "11-10 13:08");
        assert_eq!(
            format("YYYY/MM/DD ddd hh:mm:ss A [UTC+8]", &time).unwrap(),
            "2026/11/10 周二 01:08:07 PM UTC+8"
        );
        assert!(format("QQ HH:mm", &time).is_err());
        assert!(format("[bad", &time).is_err());
    }
}
