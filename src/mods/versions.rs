// Numeric Minecraft releases. Unknown expressions stay advisory.
use std::cmp::Ordering;
fn parts(s: &str) -> Option<Vec<u32>> {
    s.trim().split('.').map(|s| s.parse().ok()).collect()
}
fn compare(a: &[u32], b: &[u32]) -> Ordering {
    (0..a.len().max(b.len()))
        .map(|i| a.get(i).unwrap_or(&0).cmp(b.get(i).unwrap_or(&0)))
        .find(|v| !v.is_eq())
        .unwrap_or(Ordering::Equal)
}
fn predicate(s: &str, v: &[u32]) -> Option<bool> {
    if s == "*" {
        return Some(true);
    }
    for op in [">=", "<=", ">", "<", "=", "~", "^", ""] {
        if let Some(rest) = s.strip_prefix(op) {
            let fields = rest.split('.').collect::<Vec<_>>();
            if let Some(i) = fields.iter().position(|p| matches!(*p, "*" | "x" | "X")) {
                if !op.is_empty() || fields[i..].iter().any(|p| !matches!(*p, "*" | "x" | "X")) {
                    return None;
                }
                return Some(
                    i == 0 || compare(v.get(..i)?, &parts(&fields[..i].join("."))?).is_eq(),
                );
            }
            let b = parts(rest)?;
            let ord = compare(v, &b);
            return Some(match op {
                ">=" => !ord.is_lt(),
                "<=" => !ord.is_gt(),
                ">" => ord.is_gt(),
                "<" => ord.is_lt(),
                "~" | "^" => {
                    let i = if op == "~" {
                        usize::from(b.len() > 1)
                    } else {
                        b.iter().position(|x| *x != 0).unwrap_or(b.len() - 1)
                    };
                    let mut upper = b[..=i].to_vec();
                    upper[i] = upper[i].checked_add(1)?;
                    !ord.is_lt() && compare(v, &upper).is_lt()
                }
                _ => ord.is_eq(),
            });
        }
    }
    None
}
pub(super) fn matches(expr: &str, version: &str) -> Option<bool> {
    let s = expr.trim();
    let v = parts(version)?;
    if s.starts_with(['[', '(']) {
        let mut tail = s;
        let mut ok = false;
        while !tail.is_empty() {
            let last = tail.find([']', ')'])?;
            let segment = &tail[..=last];
            if segment.len() < 3 || !segment.starts_with(['[', '(']) {
                return None;
            }
            let inside = &segment[1..last];
            if let Some((a, b)) = inside.split_once(',') {
                let low = if a.trim().is_empty() {
                    true
                } else {
                    let c = compare(&v, &parts(a)?);
                    c.is_gt() || (c.is_eq() && segment.starts_with('['))
                };
                let high = if b.trim().is_empty() {
                    true
                } else {
                    let c = compare(&v, &parts(b)?);
                    c.is_lt() || (c.is_eq() && segment.ends_with(']'))
                };
                ok |= low && high;
            } else {
                ok |= compare(&v, &parts(inside)?).is_eq();
            }
            tail = tail[last + 1..].trim();
            if !tail.is_empty() {
                tail = tail.strip_prefix(',')?.trim();
            }
        }
        return Some(ok);
    }
    let mut unknown = false;
    for group in s.split("||") {
        let mut result = Some(true);
        if group.trim().is_empty() {
            return None;
        }
        for word in group.split_whitespace() {
            match predicate(word, &v) {
                Some(false) => {
                    result = Some(false);
                    break;
                }
                None => result = None,
                _ => {}
            }
        }
        if result == Some(true) {
            return result;
        }
        unknown |= result.is_none();
    }
    if unknown { None } else { Some(false) }
}
