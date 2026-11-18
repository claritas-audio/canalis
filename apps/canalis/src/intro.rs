//! The startup intro, as on the website: the c of the Canalis logo is a cable whose plug is
//! carried round and pushed into the top node once PipeWire answers; then the signal flows
//! through it and the window shows. Any click or key skips it. It plays on every window
//! but the ones the tray opens.
use std::time::Instant;

use eframe::egui::{self, Color32, Pos2, Stroke, Vec2, pos2, vec2};

use crate::i18n::tf;
use crate::theme;

const LOGO: &str = include_str!("../../../assets/canalis-logo.svg");
const YELLOW: Color32 = Color32::from_rgb(0xf2, 0xc9, 0x4c);
const CABLE: Color32 = Color32::from_rgb(0x5a, 0x60, 0x6c);
const PLUG: Color32 = Color32::from_rgb(0x8a, 0x90, 0x9c);
/// The logo's width on screen, in points, unless the window is narrower.
const LOGO_WIDTH: f32 = 380.0;

// Seconds. Carry the plug in front of the socket, hold a beat, push it home; the click, the
// signal, a moment to read the logo, then the window.
const CARRY: f32 = 1.1;
const HOLD: f32 = 0.0;
const PUSH: f32 = 0.16;
const FLOW: f32 = 0.7;
const SIGNAL_RING: f32 = 0.6;
const LINGER: f32 = 0.45;
const FADE: f32 = 0.4;

/// Set by the tray on the windows it opens: a click there wants the window at once.
pub const FROM_TRAY: &str = "CANALIS_FROM_TRAY";

pub struct Intro {
    logo: Logo,
    cable: Cable,
    started: Instant,
    /// When the plug is pushed home: after the carry and the hold, once PipeWire answers.
    push_at: Option<f32>,
    /// `CANALIS_SCREENSHOT_INTRO=<seconds>` freezes the intro there, for screenshots.
    frozen: Option<f32>,
}

impl Intro {
    pub fn new(frozen: Option<f32>) -> Option<Intro> {
        let logo = Logo::parse(LOGO)?;
        let cable = Cable::new(&logo)?;
        Some(Intro {
            logo,
            cable,
            started: Instant::now(),
            push_at: frozen.map(|_| CARRY + HOLD),
            frozen,
        })
    }

    /// Paints the intro over the window; false once it is over.
    pub fn show(&mut self, ctx: &egui::Context, connected: bool) -> bool {
        let skipped = ctx.input(|i| {
            i.pointer.any_pressed()
                || i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))
        });
        if skipped && self.frozen.is_none() {
            return false;
        }
        let t = self
            .frozen
            .unwrap_or_else(|| self.started.elapsed().as_secs_f32());
        if self.push_at.is_none() && connected && t >= CARRY + HOLD {
            self.push_at = Some(t);
        }
        let plugged = self.push_at.map(|p| p + PUSH);
        let flowed = plugged.map(|p| p + FLOW);
        let fade = flowed.map_or(0.0, |f| ((t - f - LINGER) / FADE).clamp(0.0, 1.0));
        if fade >= 1.0 {
            return false;
        }
        ctx.request_repaint();

        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("intro"),
        ));
        painter.rect_filled(
            screen,
            0.0,
            fade_color(theme::BACKGROUND, 1.0 - smooth(fade)),
        );
        // The logo leaves first: translucent, its overlapping caps would show as dots.
        let alpha = 1.0 - smooth((fade * 2.5).min(1.0));
        let scale = LOGO_WIDTH.min(screen.width() * 0.6) / self.logo.size.x;
        let origin = screen.center() - self.logo.size * scale / 2.0;
        let at = |p: Pos2| origin + p.to_vec2() * scale;
        let line = |pts: &[Pos2], width: f32, color: Color32| {
            polyline(
                &painter,
                pts.iter().map(|&p| at(p)).collect(),
                width * scale,
                fade_color(color, alpha),
            );
        };

        for (i, path) in self.logo.paths.iter().enumerate() {
            if i != self.cable.path {
                line(&path.pts, path.width, path.color);
            }
        }
        let width = self.logo.paths[self.cable.path].width;
        match plugged {
            Some(plugged) if t >= plugged => {
                let cable = &self.cable.pts;
                let flow = ((t - plugged) / FLOW).clamp(0.0, 1.0);
                line(cable, width, if flow >= 1.0 { YELLOW } else { CABLE });
                if flow > 0.0 && flow < 1.0 {
                    line(&prefix(cable, smooth(flow)), width, YELLOW);
                }
            }
            _ => {
                let pts = self.cable.at(t, self.push_at);
                line(&pts, width, CABLE);
                // The plug: a short thick head on the cable's end, along its heading.
                let end = pts[pts.len() - 1];
                let dir = (end - pts[pts.len() - 4]).normalized();
                line(&[end - dir * 0.12, end], 0.2, PLUG);
            }
        }

        for dot in &self.logo.dots {
            painter.circle_filled(at(dot.center), dot.r * scale, fade_color(dot.color, alpha));
        }
        // The byline, far below the logo; it eases in so the cable is seen first.
        let byline = alpha * smooth((t / 0.6).min(1.0));
        let mut job = egui::text::LayoutJob::default();
        let font = egui::FontId::proportional(15.0);
        let author = tf("by {author}", &[("author", &"Luca Giulianini")]);
        for (text, color) in [
            (author.as_str(), theme::TEXT_DIM),
            (" · meltingbots", theme::TEXT_DIM.gamma_multiply(0.6)),
        ] {
            let format = egui::TextFormat::simple(font.clone(), fade_color(color, byline));
            job.append(text, 0.0, format);
        }
        let galley = painter.layout_job(job);
        let size = galley.size();
        let corner = pos2(
            screen.center().x - size.x / 2.0,
            screen.bottom() - 36.0 - size.y,
        );
        painter.galley(corner, galley, theme::TEXT_DIM);
        // The signal reaching the top node rings out from it.
        if let Some(flowed) = flowed {
            let k = (t - flowed) / SIGNAL_RING;
            if (0.0..1.0).contains(&k) {
                let r = (0.13 + 0.3 * (1.0 - (1.0 - k).powi(2))) * scale;
                let c = fade_color(YELLOW, alpha * (1.0 - k));
                painter.circle_stroke(at(self.cable.socket()), r, Stroke::new(0.04 * scale, c));
            }
        }
        true
    }
}

fn fade_color(c: Color32, alpha: f32) -> Color32 {
    c.gamma_multiply(alpha)
}

/// The logo is drawn with round caps; egui strokes end square, so a dot covers each end.
fn polyline(painter: &egui::Painter, pts: Vec<Pos2>, width: f32, color: Color32) {
    let (first, last) = (pts[0], pts[pts.len() - 1]);
    painter.add(egui::Shape::line(pts, Stroke::new(width, color)));
    painter.circle_filled(first, width / 2.0, color);
    painter.circle_filled(last, width / 2.0, color);
}

fn smooth(k: f32) -> f32 {
    k * k * (3.0 - 2.0 * k)
}

/// The first `share` of a polyline, by length.
fn prefix(pts: &[Pos2], share: f32) -> Vec<Pos2> {
    let total: f32 = pts.windows(2).map(|w| w[0].distance(w[1])).sum();
    let mut left = total * share;
    let mut out = vec![pts[0]];
    for w in pts.windows(2) {
        let d = w[0].distance(w[1]);
        if d >= left {
            out.push(w[0] + (w[1] - w[0]) * (left / d.max(f32::EPSILON)));
            break;
        }
        left -= d;
        out.push(w[1]);
    }
    out
}

/// The c as a cable from its bottom node to its top one, walked like a turtle so it can curl.
struct Cable {
    /// The c's index among the logo's paths.
    path: usize,
    /// The c from the bottom node up.
    pts: Vec<Pos2>,
    lens: Vec<f32>,
    heads: Vec<f32>,
    turns: Vec<f32>,
}

impl Cable {
    fn new(logo: &Logo) -> Option<Cable> {
        // The c is the tallest yellow stroke; the logo also carries the small Claritas mark.
        let height = |p: &Path| {
            let (lo, hi) = p.pts.iter().fold((f32::MAX, f32::MIN), |(lo, hi), q| {
                (lo.min(q.y), hi.max(q.y))
            });
            hi - lo
        };
        let path = (0..logo.paths.len())
            .filter(|&i| logo.paths[i].color == YELLOW)
            .max_by(|&a, &b| height(&logo.paths[a]).total_cmp(&height(&logo.paths[b])))?;
        // The SVG draws it from the top node down; the cable leaves from the bottom.
        let pts: Vec<Pos2> = logo.paths[path].pts.iter().rev().copied().collect();
        let steps: Vec<Vec2> = pts.windows(2).map(|w| w[1] - w[0]).collect();
        let lens = steps.iter().map(|s| s.length()).collect();
        let heads: Vec<f32> = steps.iter().map(|s| s.y.atan2(s.x)).collect();
        let turns = (0..heads.len())
            .map(|i| {
                if i == 0 {
                    0.0
                } else {
                    wrap(heads[i] - heads[i - 1])
                }
            })
            .collect();
        Some(Cable {
            path,
            pts,
            lens,
            heads,
            turns,
        })
    }

    fn socket(&self) -> Pos2 {
        self.pts[self.pts.len() - 1]
    }

    /// The cable at `t`: its own curl, then its end pulled to wherever the plug is held.
    fn at(&self, t: f32, push_at: Option<f32>) -> Vec<Pos2> {
        let curl = 1.0 - 0.75 * (-t / 0.2).exp() * (std::f32::consts::TAU * t).cos();
        let length = 0.2 + 0.8 * (1.0 - (1.0 - (t / 0.95).min(1.0)).powi(3));
        let wobble = 0.03 * (-t / 0.4).exp();
        let mut pts = self.body(curl, length, wobble, t * 1.6);
        let end = pts[pts.len() - 1];
        let hand = self.hand(t, push_at);
        let n = pts.len() - 1;
        for (i, p) in pts.iter_mut().enumerate() {
            *p += (hand - end) * smooth(i as f32 / n as f32).powi(2);
        }
        pts
    }

    fn body(&self, curl: f32, length: f32, wobble: f32, phase: f32) -> Vec<Pos2> {
        let n = self.lens.len() as f32;
        let mut p = self.pts[0];
        let mut h = self.heads[0];
        let mut out = vec![p];
        for (i, (&len, &turn)) in self.lens.iter().zip(&self.turns).enumerate() {
            let u = i as f32 / n;
            h += turn * curl + wobble * u * (std::f32::consts::TAU * (u * 1.5 - phase)).sin();
            p += vec2(h.cos(), h.sin()) * len * length;
            out.push(p);
        }
        out
    }

    /// Where the plug is held: swung up and over to the front of the socket, then pushed in
    /// along the c's own heading at its end.
    fn hand(&self, t: f32, push_at: Option<f32>) -> Pos2 {
        let socket = self.socket();
        let heading = self.heads[self.heads.len() - 1];
        let front = socket - vec2(heading.cos(), heading.sin()) * 0.3;
        if t < CARRY {
            let from = self.body(0.25, 0.2, 0.0, 0.0)[self.lens.len()];
            let via = pos2((from.x + front.x) / 2.0 + 0.15, from.y.min(front.y) - 0.45);
            // Starts gently, arrives still moving, so the push follows without a stop.
            let x = t / CARRY;
            let k = x * x * (2.0 - x);
            let (a, b, c) = ((1.0 - k).powi(2), 2.0 * k * (1.0 - k), k * k);
            return pos2(
                a * from.x + b * via.x + c * front.x,
                a * from.y + b * via.y + c * front.y,
            );
        }
        let k = push_at.map_or(0.0, |p| ((t - p) / PUSH).clamp(0.0, 1.0).powi(2));
        front + (socket - front) * k
    }
}

fn wrap(mut a: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    while a > PI {
        a -= TAU;
    }
    while a < -PI {
        a += TAU;
    }
    a
}

/// The logo SVG as drawn by `scripts/`: stroked polylines and filled dots, nothing else.
struct Logo {
    size: Vec2,
    paths: Vec<Path>,
    dots: Vec<Dot>,
}

struct Path {
    color: Color32,
    width: f32,
    pts: Vec<Pos2>,
}

struct Dot {
    color: Color32,
    center: Pos2,
    r: f32,
}

impl Logo {
    fn parse(svg: &str) -> Option<Logo> {
        let view: Vec<f32> = attr(svg, "viewBox")?
            .split(' ')
            .filter_map(|v| v.parse().ok())
            .collect();
        let mut logo = Logo {
            size: vec2(*view.get(2)?, *view.get(3)?),
            paths: Vec::new(),
            dots: Vec::new(),
        };
        let num = |tag: &str, name: &str| attr(tag, name)?.parse::<f32>().ok();
        for tag in svg.split('<').skip(1) {
            if tag.starts_with("path") {
                let nums: Vec<f32> = attr(tag, "d")?
                    .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                    .filter_map(|v| v.parse().ok())
                    .collect();
                logo.paths.push(Path {
                    color: hex(attr(tag, "stroke")?)?,
                    width: num(tag, "stroke-width")?,
                    pts: nums
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|&[x, y]| pos2(x, y))
                        .collect(),
                });
            } else if tag.starts_with("circle") {
                logo.dots.push(Dot {
                    color: hex(attr(tag, "fill")?)?,
                    center: pos2(num(tag, "cx")?, num(tag, "cy")?),
                    r: num(tag, "r")?,
                });
            }
        }
        Some(logo)
    }
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let len = tag[start..].find('"')?;
    Some(&tag[start..start + len])
}

fn hex(text: &str) -> Option<Color32> {
    let v = u32::from_str_radix(text.strip_prefix('#')?, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_logo_parses_and_the_cable_ends_on_its_node() {
        let logo = Logo::parse(LOGO).expect("logo parses");
        let cable = Cable::new(&logo).expect("the c is found");
        assert!(
            logo.dots
                .iter()
                .any(|d| d.center.distance(cable.socket()) < 0.01)
        );
        // Fully curled, at full length and still, the walk lands on the c itself.
        let walked = cable.body(1.0, 1.0, 0.0, 0.0);
        assert!(walked[walked.len() - 1].distance(cable.socket()) < 1e-3);
    }
}
