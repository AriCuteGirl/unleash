//! Theme engine: reads the active hypr-rice and turns it into design tokens and GTK CSS.
//!
//! Source chain (all optional, each step falls back to the built-in Lucid Blocks palette):
//!   ~/.local/state/hypr-rices/current          -> active rice id
//!   ~/.local/share/hypr-rices/<id>/rice.json   -> colors, theme.accent/accent2, noctalia_scheme
//!   ~/.config/noctalia/palettes/<Name>.json    -> semantic surface/text/outline colours
//!   <rice>/hypr/look.lua                       -> rounding, shadow offset/sharpness
//!
//! Everything here is pure file reading plus string building, so it runs off the UI thread
//! and is unit-tested with fixture directories.

use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn parse(s: &str) -> Option<Rgb> {
        let h = s.trim().trim_start_matches('#');
        if h.len() < 6 || !h.is_char_boundary(6) {
            return None;
        }
        let v = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        Some(Rgb(v(0)?, v(2)?, v(4)?))
    }
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
    pub fn rgba(self, a: f64) -> String {
        format!("rgba({},{},{},{:.3})", self.0, self.1, self.2, a.clamp(0.0, 1.0))
    }
    /// Linear mix towards `other` by `t` (0 = self, 1 = other).
    pub fn mix(self, other: Rgb, t: f64) -> Rgb {
        let m = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }
    pub fn luminance(self) -> f64 {
        let c = |v: u8| {
            let x = v as f64 / 255.0;
            if x <= 0.03928 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * c(self.0) + 0.7152 * c(self.1) + 0.0722 * c(self.2)
    }
    pub fn contrast(self, other: Rgb) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
}

const BLACK: Rgb = Rgb(0, 0, 0);
const WHITE: Rgb = Rgb(255, 255, 255);

/// Design tokens. Names follow the spec's table.
#[derive(Clone, Debug, PartialEq)]
pub struct Tokens {
    pub rice_id: String,
    pub rice_name: String,
    pub bg_deep: Rgb,     // sidebar / deepest layer
    pub bg: Rgb,          // window background
    pub surface: Rgb,     // cards, inputs, rows
    pub surface_alt: Rgb, // hover / raised
    pub wine: Rgb,        // subtle fills, selection background, strokes
    pub crimson: Rgb,     // borders, focus rings
    pub accent: Rgb,      // primary buttons
    pub accent_hot: Rgb,  // active highlights, badges
    pub on_accent: Rgb,
    pub text: Rgb,
    pub text_dim: Rgb,
    pub text_faint: Rgb,
    pub danger: Rgb,
    pub success: Rgb,
    pub radius: u32,          // from Hyprland rounding
    pub shadow_x: i32,
    pub shadow_y: i32,
    pub shadow_sharp: bool,
    pub shadow_color: String, // css colour
}

impl Tokens {
    /// Built-in Lucid Blocks palette (exact values from the rice files).
    pub fn lucid_blocks() -> Tokens {
        Tokens {
            rice_id: "lucid-blocks".into(),
            rice_name: "Lucid Blocks".into(),
            bg_deep: Rgb(0x1c, 0x04, 0x08),
            bg: Rgb(0x2e, 0x0c, 0x16),
            surface: Rgb(0x3d, 0x16, 0x22),
            surface_alt: Rgb(0x4a, 0x1c, 0x2a),
            wine: Rgb(0x5e, 0x0b, 0x15),
            crimson: Rgb(0xa8, 0x23, 0x3f),
            accent: Rgb(0xbe, 0x31, 0x44),
            accent_hot: Rgb(0xbf, 0x12, 0x4d),
            on_accent: WHITE,
            text: Rgb(0xed, 0xe6, 0xe3),
            text_dim: Rgb(0xd8, 0xb4, 0xbe),
            text_faint: Rgb(0xb8, 0x94, 0x9f),
            danger: Rgb(0xff, 0x6b, 0x7a),
            success: Rgb(0x7c, 0xf2, 0x9a),
            radius: 0,
            shadow_x: 5,
            shadow_y: 5,
            shadow_sharp: true,
            shadow_color: "rgba(0,0,0,0.93)".into(),
        }
    }
}

#[derive(Deserialize, Default)]
struct RiceJson {
    id: Option<String>,
    name: Option<String>,
    #[serde(default)]
    colors: Vec<String>,
    theme: Option<RiceTheme>,
    noctalia_scheme: Option<Vec<String>>,
}
#[derive(Deserialize, Default)]
struct RiceTheme {
    accent: Option<String>,
    accent2: Option<String>,
}
#[derive(Deserialize)]
struct NoctaliaFile {
    dark: Option<Noctalia>,
}
#[derive(Deserialize, Default)]
#[allow(non_snake_case)]
struct Noctalia {
    mSurface: Option<String>,
    mOnSurface: Option<String>,
    mSurfaceVariant: Option<String>,
    mOnSurfaceVariant: Option<String>,
    mPrimary: Option<String>,
    mOnPrimary: Option<String>,
    mOnSecondary: Option<String>,
    mError: Option<String>,
    mOutline: Option<String>,
    mHover: Option<String>,
}

/// Where the rice switcher keeps things. Overridable for tests.
#[derive(Clone, Debug)]
pub struct RicePaths {
    pub state: PathBuf,    // ~/.local/state/hypr-rices
    pub rices: PathBuf,    // ~/.local/share/hypr-rices
    pub noctalia: PathBuf, // ~/.config/noctalia/palettes
}

impl RicePaths {
    pub fn from_env() -> RicePaths {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let env = |k: &str, d: &str| {
            std::env::var_os(k).map(PathBuf::from).unwrap_or_else(|| home.join(d))
        };
        RicePaths {
            state: env("XDG_STATE_HOME", ".local/state").join("hypr-rices"),
            rices: env("XDG_DATA_HOME", ".local/share").join("hypr-rices"),
            noctalia: env("XDG_CONFIG_HOME", ".config").join("noctalia/palettes"),
        }
    }
    pub fn current_file(&self) -> PathBuf {
        self.state.join("current")
    }
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() < 128
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !id.starts_with('.')
}

/// Load tokens for the active rice. Never fails: unknown pieces fall back to Lucid Blocks.
pub fn load_active(paths: &RicePaths) -> Tokens {
    let id = fs::read_to_string(paths.current_file()).unwrap_or_default();
    let id = id.trim();
    if !is_safe_id(id) {
        return Tokens::lucid_blocks();
    }
    load_rice(paths, id)
}

/// The look for the config's `colors`: "rice" (or anything that isn't installed) follows the
/// active rice, a rice id keeps that rice.
pub fn load_choice(paths: &RicePaths, choice: &str) -> Tokens {
    match pinned_id(paths, choice) {
        Some(id) => load_rice(paths, &id),
        None => load_active(paths),
    }
}

/// The rice id `choice` pins, if there is such a rice.
pub fn pinned_id(paths: &RicePaths, choice: &str) -> Option<String> {
    let id = choice.trim();
    (id != "rice" && is_safe_id(id) && paths.rices.join(id).join("rice.json").is_file()).then(|| id.to_string())
}

/// Installed rices as (id, name), sorted by name.
pub fn list_rices(paths: &RicePaths) -> Vec<(String, String)> {
    let Ok(rd) = fs::read_dir(&paths.rices) else { return vec![] };
    let mut v: Vec<(String, String)> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|id| is_safe_id(id) && id != "rice")
        .filter_map(|id| {
            let rice = serde_json_lite(&fs::read_to_string(paths.rices.join(&id).join("rice.json")).ok()?)?;
            let name = rice.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| id.clone());
            Some((id, name))
        })
        .collect();
    v.sort_by(|a, b| crate::util::natural_cmp(&a.1, &b.1));
    v
}

pub fn load_rice(paths: &RicePaths, id: &str) -> Tokens {
    let mut t = Tokens::lucid_blocks();
    if !is_safe_id(id) {
        return t;
    }
    let dir = paths.rices.join(id);
    let Some(rice) = fs::read_to_string(dir.join("rice.json"))
        .ok()
        .and_then(|s| serde_json_lite(&s))
    else {
        return t;
    };
    t.rice_id = rice.id.clone().unwrap_or_else(|| id.to_string());
    t.rice_name = rice.name.clone().unwrap_or_else(|| t.rice_id.clone());

    let col = |i: usize| rice.colors.get(i).and_then(|c| Rgb::parse(c));
    let theme = rice.theme.unwrap_or_default();
    let accent = theme.accent.as_deref().and_then(Rgb::parse).or(col(1));
    let accent2 = theme.accent2.as_deref().and_then(Rgb::parse).or(accent);

    let noc = rice
        .noctalia_scheme
        .as_ref()
        .and_then(|s| s.get(1))
        .filter(|n| is_safe_id(n))
        .and_then(|n| fs::read_to_string(paths.noctalia.join(format!("{n}.json"))).ok())
        .and_then(|s| toml_or_json_noctalia(&s))
        .unwrap_or_default();
    let n = |v: &Option<String>| v.as_deref().and_then(Rgb::parse);

    // Surfaces: Noctalia first, then the rice colour list.
    let surface0 = n(&noc.mSurface).or(col(0)).unwrap_or(t.bg);
    t.bg = surface0;
    // Deepest layer: colors[0] if it is darker than the surface, else a darkened surface.
    t.bg_deep = match col(0) {
        Some(c) if c.luminance() < surface0.luminance() * 0.92 => c,
        _ => surface0.mix(BLACK, 0.35),
    };
    t.surface = n(&noc.mSurfaceVariant).unwrap_or_else(|| surface0.mix(WHITE, 0.06));
    t.surface_alt = n(&noc.mHover).unwrap_or_else(|| t.surface.mix(WHITE, 0.06));
    t.text = n(&noc.mOnSurface).or(rice.colors.last().and_then(|c| Rgb::parse(c))).unwrap_or(t.text);
    t.text_dim = n(&noc.mOnSurfaceVariant).unwrap_or_else(|| t.text.mix(t.bg, 0.25));
    t.text_faint = n(&noc.mOnSecondary).unwrap_or_else(|| t.text.mix(t.bg, 0.42));
    if let Some(a) = accent {
        t.accent = a;
    }
    t.accent_hot = accent2.unwrap_or(t.accent);
    t.crimson = n(&noc.mPrimary).unwrap_or(t.accent);
    t.wine = n(&noc.mOutline).unwrap_or_else(|| t.accent.mix(t.bg, 0.6));
    if let Some(e) = n(&noc.mError) {
        // Make sure the error colour is readable on the window background.
        t.danger = if e.contrast(t.bg) >= 4.5 { e } else { e.mix(WHITE, 0.35) };
    }
    t.on_accent = n(&noc.mOnPrimary)
        .filter(|c| c.contrast(t.accent) >= 4.5)
        .unwrap_or_else(|| if WHITE.contrast(t.accent) >= BLACK.contrast(t.accent) { WHITE } else { BLACK });
    ensure_text_contrast(&mut t);

    if let Ok(look) = fs::read_to_string(dir.join("hypr/look.lua")) {
        apply_hypr_look(&mut t, &look);
    }
    t
}

/// WCAG AA: 4.5:1 for normal text on every surface it sits on.
fn ensure_text_contrast(t: &mut Tokens) {
    for _ in 0..20 {
        let worst = t.text_faint.contrast(t.surface_alt).min(t.text_faint.contrast(t.bg));
        if worst >= 4.5 {
            break;
        }
        t.text_faint = t.text_faint.mix(t.text, 0.2);
    }
    for _ in 0..20 {
        if t.text_dim.contrast(t.surface_alt) >= 4.5 {
            break;
        }
        t.text_dim = t.text_dim.mix(WHITE, 0.15);
    }
}

/// serde_json is not a dependency; rice.json is plain JSON, which TOML can't read,
/// so parse it with a tiny JSON->toml::Value converter.
fn serde_json_lite(s: &str) -> Option<RiceJson> {
    json::parse(s)?.try_into().ok()
}
fn toml_or_json_noctalia(s: &str) -> Option<Noctalia> {
    let f: NoctaliaFile = json::parse(s)?.try_into().ok()?;
    f.dark
}

/// Reads `rounding = N`, `sharp = bool`, `offset = { x, y }` and the shadow `color` from a
/// hypr-rice look.lua. Only the `shadow = {...}` block is used for shadow values.
pub fn apply_hypr_look(t: &mut Tokens, lua: &str) {
    let strip = |l: &str| l.split("--").next().unwrap_or("").trim().to_string();
    let lines: Vec<String> = lua.lines().map(strip).collect();
    let num = |l: &str, key: &str| -> Option<f64> {
        let rest = l.strip_prefix(key)?.trim_start().strip_prefix('=')?;
        rest.trim().trim_end_matches(',').trim().parse().ok()
    };
    let mut in_shadow = false;
    for l in &lines {
        if let Some(r) = num(l, "rounding") {
            t.radius = r.clamp(0.0, 32.0) as u32;
        }
        if l.starts_with("shadow") && l.contains('{') {
            in_shadow = true;
            continue;
        }
        if in_shadow {
            if l.starts_with('}') {
                in_shadow = false;
                continue;
            }
            if let Some(v) = l.strip_prefix("sharp").and_then(|r| r.trim_start().strip_prefix('=')) {
                t.shadow_sharp = v.trim().trim_end_matches(',').trim() == "true";
            }
            if let Some(v) = l.strip_prefix("offset").and_then(|r| r.trim_start().strip_prefix('=')) {
                let nums: Vec<i32> = v
                    .trim_matches(|c: char| c == '{' || c == '}' || c == ',' || c.is_whitespace())
                    .split(',')
                    .filter_map(|n| n.trim().trim_end_matches('}').trim().parse::<f64>().ok())
                    .map(|n| n.clamp(-20.0, 20.0) as i32)
                    .collect();
                if nums.len() == 2 {
                    t.shadow_x = nums[0];
                    t.shadow_y = nums[1];
                }
            }
            if let Some(v) = l.strip_prefix("color").and_then(|r| r.trim_start().strip_prefix('=')) {
                if let Some(c) = hypr_color(v) {
                    t.shadow_color = c;
                }
            }
        }
    }
}

/// "rgba(000000EE)" / "rgb(BE3144)" -> css rgba()
fn hypr_color(v: &str) -> Option<String> {
    let v = v.trim().trim_end_matches(',').trim().trim_matches('"');
    let inner = v.strip_prefix("rgba(").or_else(|| v.strip_prefix("rgb("))?.strip_suffix(')')?;
    let c = Rgb::parse(inner)?;
    let a = if inner.len() >= 8 {
        u8::from_str_radix(&inner[6..8], 16).ok()? as f64 / 255.0
    } else {
        1.0
    };
    Some(c.rgba(a))
}

/// Papirus folder colour variants and their front colour, used to tint folder icons to the rice.
const PAPIRUS_FOLDERS: &[(&str, &str)] = &[
    ("red", "#e25252"), ("carmine", "#a30002"), ("pink", "#f06292"), ("magenta", "#ca71df"),
    ("violet", "#7e57c2"), ("deeporange", "#eb6637"), ("brown", "#ae8e6c"), ("black", "#4f4f4f"),
    ("grey", "#8e8e8e"), ("indigo", "#5c6bc0"), ("blue", "#5294e2"), ("cyan", "#00bcd4"),
    ("teal", "#16a085"), ("green", "#87b158"), ("yellow", "#f9bd30"), ("orange", "#ee923a"),
    ("white", "#e4e4e4"), ("nordic", "#81a1c1"), ("bluegrey", "#607d8b"), ("palebrown", "#d1bfae"),
    ("paleorange", "#eeca8f"), ("darkcyan", "#45abb7"),
];

/// Nearest Papirus folder colour (weighted RGB distance) to the rice's primary colour.
pub fn papirus_folder_color(t: &Tokens) -> &'static str {
    let c = t.crimson;
    let d = |h: &str| {
        let o = Rgb::parse(h).unwrap();
        let (dr, dg, db) = (c.0 as f64 - o.0 as f64, c.1 as f64 - o.1 as f64, c.2 as f64 - o.2 as f64);
        let rm = (c.0 as f64 + o.0 as f64) / 2.0;
        (2.0 + rm / 256.0) * dr * dr + 4.0 * dg * dg + (2.0 + (255.0 - rm) / 256.0) * db * db
    };
    PAPIRUS_FOLDERS
        .iter()
        .min_by(|a, b| d(a.1).total_cmp(&d(b.1)))
        .map(|(n, _)| *n)
        .unwrap_or("red")
}

pub fn papirus_color_names() -> Vec<&'static str> {
    PAPIRUS_FOLDERS.iter().map(|(n, _)| *n).collect()
}

pub fn is_papirus_color(name: &str) -> bool {
    PAPIRUS_FOLDERS.iter().any(|(n, _)| *n == name)
}

/// User overrides from config.toml `[appearance]`.
#[derive(Clone, Debug)]
pub struct Appearance {
    pub radius: Option<u32>, // None = follow rice
    pub opacity: f64,
    pub font: String,
    pub reduced_motion: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance { radius: None, opacity: 0.94, font: "Google Sans Flex".into(), reduced_motion: false }
    }
}

/// Builds the whole app stylesheet from tokens.
pub fn css(t: &Tokens, a: &Appearance) -> String {
    let r = a.radius.unwrap_or(t.radius);
    // Small controls use a slightly smaller radius than panels, like the switcher UI.
    let r_small = if r == 0 { 0 } else { r.saturating_sub(4).max(4) };
    let r_big = if r == 0 { 0 } else { r + 4 };
    let blur = if t.shadow_sharp { 0 } else { 14 };
    let shadow = format!("{}px {}px {}px 0 {}", t.shadow_x, t.shadow_y, blur, t.shadow_color);
    let op = a.opacity.clamp(0.3, 1.0);
    let motion = if a.reduced_motion { "0ms" } else { "120ms" };
    let font = a.font.replace(['"', ';', '{', '}'], "");
    let mut s = String::new();
    let vars = [
        ("bg_deep", t.bg_deep.hex()),
        ("bg", t.bg.hex()),
        ("surface", t.surface.hex()),
        ("surface_alt", t.surface_alt.hex()),
        ("wine", t.wine.hex()),
        ("crimson", t.crimson.hex()),
        ("accent", t.accent.hex()),
        ("accent_hot", t.accent_hot.hex()),
        ("on_accent", t.on_accent.hex()),
        ("text", t.text.hex()),
        ("text_dim", t.text_dim.hex()),
        ("text_faint", t.text_faint.hex()),
        ("danger", t.danger.hex()),
        ("success", t.success.hex()),
        ("win_bg", t.bg.rgba(op)),
        ("side_bg", t.bg_deep.rgba((op + 0.06).min(1.0))),
        ("sel_bg", t.wine.rgba(0.85)),
        ("hover_bg", t.surface_alt.rgba(0.75)),
        // GTK's own widgets (focus rings, checkboxes, selections) use these names
        ("accent_color", t.accent_hot.hex()),
        ("accent_bg_color", t.accent.hex()),
        ("accent_fg_color", t.on_accent.hex()),
        ("theme_selected_bg_color", t.wine.hex()),
        ("theme_selected_fg_color", t.text.hex()),
    ];
    for (k, v) in vars {
        s.push_str(&format!("@define-color {k} {v};\n"));
    }
    s.push_str(&format!(
        "@define-color block_shadow_c {};\n",
        t.shadow_color
    ));
    s.push_str(
        &BASE_CSS
            .replace("$R_SMALL", &format!("{r_small}px"))
            .replace("$R_BIG", &format!("{r_big}px"))
            .replace("$R", &format!("{r}px"))
            .replace("$SHADOW", &shadow)
            .replace("$MOTION", motion)
            .replace("$FONT", &font),
    );
    s
}

const BASE_CSS: &str = include_str!("style.css");

/// Path of the look file, for the file monitor.
pub fn rice_dir(paths: &RicePaths, id: &str) -> Option<PathBuf> {
    is_safe_id(id).then(|| paths.rices.join(id))
}

pub fn active_id(paths: &RicePaths) -> Option<String> {
    let id = fs::read_to_string(paths.current_file()).ok()?;
    let id = id.trim().to_string();
    is_safe_id(&id).then_some(id)
}

#[allow(dead_code)]
pub fn exists(p: &Path) -> bool {
    p.exists()
}

/// Minimal JSON reader into toml::Value (objects, arrays, strings, numbers, bools; null dropped).
mod json {
    use toml::Value;
    pub fn parse(s: &str) -> Option<Value> {
        let mut p = P { b: s.as_bytes(), i: 0 };
        let v = p.value()?;
        p.ws();
        (p.i == p.b.len()).then_some(v?)
    }
    struct P<'a> {
        b: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
        }
        fn eat(&mut self, c: u8) -> Option<()> {
            self.ws();
            (self.b.get(self.i) == Some(&c)).then(|| self.i += 1)
        }
        // Outer Option = syntax ok, inner Option = value (None for null).
        fn value(&mut self) -> Option<Option<Value>> {
            self.ws();
            match *self.b.get(self.i)? {
                b'{' => {
                    self.i += 1;
                    let mut m = toml::map::Map::new();
                    if self.eat(b'}').is_some() {
                        return Some(Some(Value::Table(m)));
                    }
                    loop {
                        self.ws();
                        let k = self.string()?;
                        self.eat(b':')?;
                        if let Some(v) = self.value()? {
                            m.insert(k, v);
                        }
                        if self.eat(b',').is_some() {
                            continue;
                        }
                        self.eat(b'}')?;
                        return Some(Some(Value::Table(m)));
                    }
                }
                b'[' => {
                    self.i += 1;
                    let mut a = Vec::new();
                    if self.eat(b']').is_some() {
                        return Some(Some(Value::Array(a)));
                    }
                    loop {
                        if let Some(v) = self.value()? {
                            a.push(v);
                        }
                        if self.eat(b',').is_some() {
                            continue;
                        }
                        self.eat(b']')?;
                        return Some(Some(Value::Array(a)));
                    }
                }
                b'"' => Some(Some(Value::String(self.string()?))),
                b't' => self.lit("true").map(|_| Some(Value::Boolean(true))),
                b'f' => self.lit("false").map(|_| Some(Value::Boolean(false))),
                b'n' => self.lit("null").map(|_| None),
                _ => {
                    let st = self.i;
                    while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                        self.i += 1;
                    }
                    let n = std::str::from_utf8(&self.b[st..self.i]).ok()?;
                    if let Ok(i) = n.parse::<i64>() {
                        Some(Some(Value::Integer(i)))
                    } else {
                        n.parse::<f64>().ok().map(|f| Some(Value::Float(f)))
                    }
                }
            }
        }
        fn lit(&mut self, w: &str) -> Option<()> {
            self.b[self.i..].starts_with(w.as_bytes()).then(|| self.i += w.len())
        }
        fn string(&mut self) -> Option<String> {
            if self.b.get(self.i) != Some(&b'"') {
                return None;
            }
            self.i += 1;
            let mut out = Vec::new();
            loop {
                let c = *self.b.get(self.i)?;
                self.i += 1;
                match c {
                    b'"' => return String::from_utf8(out).ok(),
                    b'\\' => {
                        let e = *self.b.get(self.i)?;
                        self.i += 1;
                        match e {
                            b'n' => out.push(b'\n'),
                            b't' => out.push(b'\t'),
                            b'r' => out.push(b'\r'),
                            b'b' | b'f' => {}
                            b'u' => {
                                let h = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
                                self.i += 4;
                                let ch = char::from_u32(u32::from_str_radix(h, 16).ok()?).unwrap_or('\u{fffd}');
                                let mut buf = [0u8; 4];
                                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                            }
                            other => out.push(other),
                        }
                    }
                    _ => out.push(c),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> (PathBuf, RicePaths) {
        let root = std::env::temp_dir().join(format!("unleash-theme-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let p = RicePaths {
            state: root.join("state"),
            rices: root.join("rices"),
            noctalia: root.join("noctalia"),
        };
        for d in [&p.state, &p.rices, &p.noctalia] {
            fs::create_dir_all(d).unwrap();
        }
        (root, p)
    }

    #[test]
    fn falls_back_when_nothing_found() {
        let (_r, p) = fixture("none");
        assert_eq!(load_active(&p), Tokens::lucid_blocks());
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let (_r, p) = fixture("trav");
        fs::write(p.current_file(), "../../etc\n").unwrap();
        assert_eq!(load_active(&p).rice_id, "lucid-blocks");
    }

    #[test]
    fn reads_rice_noctalia_and_look() {
        let (_r, p) = fixture("gf");
        fs::write(p.current_file(), "gravity-falls\n").unwrap();
        let d = p.rices.join("gravity-falls");
        fs::create_dir_all(d.join("hypr")).unwrap();
        fs::write(
            d.join("rice.json"),
            r##"{"id":"gravity-falls","name":"Gravity Falls","colors":["#1E1424","#C94277"],
               "theme":{"accent":"#C94277","accent2":"#BBA0CA"},"noctalia_scheme":["custom","Ari"],"x":null}"##,
        )
        .unwrap();
        fs::write(
            p.noctalia.join("Ari.json"),
            r##"{"dark":{"mSurface":"#1E1424","mOnSurface":"#F3E8F0","mSurfaceVariant":"#2E1F36",
               "mHover":"#3A2844","mOutline":"#792359","mPrimary":"#E76D83"}}"##,
        )
        .unwrap();
        fs::write(
            d.join("hypr/look.lua"),
            "decoration = {\n rounding = 8, -- soft\n shadow = {\n sharp = false,\n offset = { 0, 0 },\n color = \"rgba(792359ff)\",\n },\n}\n",
        )
        .unwrap();
        let t = load_active(&p);
        assert_eq!(t.rice_name, "Gravity Falls");
        assert_eq!(t.accent, Rgb(0xc9, 0x42, 0x77));
        assert_eq!(t.accent_hot, Rgb(0xbb, 0xa0, 0xca));
        assert_eq!(t.surface, Rgb(0x2e, 0x1f, 0x36));
        assert_eq!(t.wine, Rgb(0x79, 0x23, 0x59));
        assert_eq!(t.radius, 8);
        assert!(!t.shadow_sharp);
        assert_eq!((t.shadow_x, t.shadow_y), (0, 0));
        assert_eq!(t.shadow_color, "rgba(121,35,89,1.000)");
        // bg_deep is derived because colors[0] == surface
        assert!(t.bg_deep.luminance() < t.bg.luminance());
    }

    #[test]
    fn colours_can_be_pinned_to_a_rice() {
        let (_r, p) = fixture("pin");
        fs::write(p.current_file(), "aaa\n").unwrap();
        for (id, name, accent) in [("aaa", "Zebra", "#112233"), ("bbb", "Apple", "#C94277")] {
            fs::create_dir_all(p.rices.join(id)).unwrap();
            let json = format!(r##"{{"id":"{id}","name":"{name}","theme":{{"accent":"{accent}"}}}}"##);
            fs::write(p.rices.join(id).join("rice.json"), json).unwrap();
        }
        fs::create_dir_all(p.rices.join("broken")).unwrap();
        assert_eq!(list_rices(&p), [("bbb".into(), "Apple".into()), ("aaa".into(), "Zebra".into())] as [(String, String); 2]);
        assert_eq!(load_choice(&p, "rice").rice_id, "aaa");
        assert_eq!(load_choice(&p, "bbb").accent, Rgb(0xc9, 0x42, 0x77));
        // a removed or odd rice follows the active one instead
        for odd in ["gone", "broken", "", "../bbb"] {
            assert_eq!(load_choice(&p, odd).rice_id, "aaa", "{odd}");
        }
    }

    #[test]
    fn lucid_meets_wcag_aa() {
        let t = Tokens::lucid_blocks();
        for bg in [t.bg, t.bg_deep, t.surface, t.surface_alt, t.wine] {
            assert!(t.text.contrast(bg) >= 4.5, "text on {}", bg.hex());
            assert!(t.text_dim.contrast(bg) >= 4.5, "dim on {}", bg.hex());
        }
        assert!(t.on_accent.contrast(t.accent) >= 4.5);
    }

    #[test]
    fn contrast_is_enforced_for_bad_palettes() {
        let (_r, p) = fixture("bad");
        fs::write(p.current_file(), "bad\n").unwrap();
        let d = p.rices.join("bad");
        fs::create_dir_all(&d).unwrap();
        fs::write(p.noctalia.join("Bad.json"), r##"{"dark":{"mSurface":"#202020","mOnSecondary":"#303030","mOnSurfaceVariant":"#404040"}}"##).unwrap();
        fs::write(d.join("rice.json"), r##"{"id":"bad","colors":["#202020","#FFFF00"],"noctalia_scheme":["custom","Bad"]}"##).unwrap();
        let t = load_active(&p);
        assert!(t.text_faint.contrast(t.bg) >= 4.5);
        assert!(t.text_dim.contrast(t.surface_alt) >= 4.5);
        assert!(t.on_accent.contrast(t.accent) >= 4.5);
    }

    #[test]
    fn folder_colour_follows_rice() {
        assert_eq!(papirus_folder_color(&Tokens::lucid_blocks()), "carmine");
        let mut gf = Tokens::lucid_blocks();
        gf.crimson = Rgb(0xe7, 0x6d, 0x83);
        assert_eq!(papirus_folder_color(&gf), "pink");
    }

    #[test]
    fn css_uses_radius_override() {
        let t = Tokens::lucid_blocks();
        let a = Appearance { radius: Some(12), ..Default::default() };
        let c = css(&t, &a);
        assert!(c.contains("border-radius: 12px"));
        assert!(c.contains("5px 5px 0px 0"));
        assert!(!c.contains('$'), "unreplaced placeholder");
    }

    #[test]
    fn json_lite_handles_escapes_and_nesting() {
        let v = json::parse(r##"{"a":[1,2.5,{"b":"x\"y\u00e9"}],"c":true,"d":null}"##).unwrap();
        assert_eq!(v["a"][2]["b"].as_str(), Some("x\"yé"));
        assert_eq!(v["c"].as_bool(), Some(true));
        assert!(json::parse("{\"a\":").is_none());
    }
}
