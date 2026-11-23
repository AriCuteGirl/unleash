//! Small helpers shared by the backend and the UI (natural sort and free names are from Burrow).


use std::cmp::Ordering;

/// Natural, case-insensitive compare: "file2" < "file10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        return natural_ascii(a.as_bytes(), b.as_bytes());
    }
    natural_unicode(a, b)
}

/// The same order without any allocation (most names): sorting a big folder calls this a lot.
fn natural_ascii(a: &[u8], b: &[u8]) -> Ordering {
    let digits = |s: &[u8], i: usize| s[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (na, nb) = (&a[i..i + digits(a, i)], &b[j..j + digits(b, j)]);
            let cut = |s: &[u8]| s.iter().take_while(|c| **c == b'0').count();
            let (ta, tb) = (&na[cut(na)..], &nb[cut(nb)..]);
            let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| na.len().cmp(&nb.len()));
            if o != Ordering::Equal {
                return o;
            }
            i += na.len();
            j += nb.len();
        } else {
            let (x, y) = (a[i].to_ascii_lowercase(), b[j].to_ascii_lowercase());
            if x != y {
                return x.cmp(&y);
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j)).then_with(|| a.cmp(b))
}

fn natural_unicode(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return a.cmp(b), // equal ignoring case: stable tie-break
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na: String = std::iter::from_fn(|| ai.next_if(|c| c.is_ascii_digit())).collect();
                let nb: String = std::iter::from_fn(|| bi.next_if(|c| c.is_ascii_digit())).collect();
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| na.len().cmp(&nb.len()));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let (lx, ly) = (x.to_lowercase().next().unwrap_or(x), y.to_lowercase().next().unwrap_or(y));
                if lx != ly {
                    return lx.cmp(&ly);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

/// Bytes with SI units, like file managers and Windows' drive view ("118 GB").
pub fn human_size(b: u64) -> String {
    const U: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", U[i])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// "name (2).ext" style free name; compound extensions like .tar.gz stay together.
pub fn free_name(name: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(name) {
        return name.to_string();
    }
    // keep compound extensions like .tar.gz together; dotfiles have no extension
    let (stem, ext) = match name.find('.').filter(|&i| i > 0) {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    };
    (2..10_000).map(|n| format!("{stem} ({n}){ext}")).find(|c| !exists(c)).unwrap_or_else(|| format!("{stem} (restored){ext}"))
}
