//! 纯 Rust 矢量图标绘制器（基于 `egui::Painter`，零字体依赖、任意 DPI 清晰锐利）。

use eframe::egui::{Color32, CornerRadius, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Server,
    Terminal,
    Plus,
    Cloud,
    Settings,
    Shield,
    Key,
    Lock,
    Edit,
    Search,
    Folder,
    Refresh,
    Trash,
    Copy,
    Theme,
    WinMin,
    WinMax,
    WinRestore,
    WinClose,
}

pub fn draw_icon(painter: &Painter, center: Pos2, size: f32, icon: Icon, color: Color32) {
    let r = size * 0.5;
    let stroke = Stroke::new(1.5_f32, color);
    let thin = Stroke::new(1.25_f32, color);

    match icon {
        Icon::Server => {
            // 上下两层服务器机架 + 状态指示灯
            let w = size * 0.82;
            let h = size * 0.32;
            let top_rect = Rect::from_center_size(Pos2::new(center.x, center.y - r * 0.42), Vec2::new(w, h));
            let bot_rect = Rect::from_center_size(Pos2::new(center.x, center.y + r * 0.42), Vec2::new(w, h));
            painter.rect_stroke(top_rect, CornerRadius::same(2), stroke, StrokeKind::Inside);
            painter.rect_stroke(bot_rect, CornerRadius::same(2), stroke, StrokeKind::Inside);
            painter.circle_filled(Pos2::new(top_rect.min.x + 3.5, top_rect.center().y), 1.3, color);
            painter.circle_filled(Pos2::new(bot_rect.min.x + 3.5, bot_rect.center().y), 1.3, color);
        }
        Icon::Terminal => {
            // `>_` 终端符号
            let x0 = center.x - r * 0.65;
            let xm = center.x - r * 0.10;
            let y_top = center.y - r * 0.45;
            let y_bot = center.y + r * 0.45;
            painter.line_segment([Pos2::new(x0, y_top), Pos2::new(xm, center.y)], stroke);
            painter.line_segment([Pos2::new(xm, center.y), Pos2::new(x0, y_bot)], stroke);
            painter.line_segment(
                [
                    Pos2::new(center.x + r * 0.05, y_bot),
                    Pos2::new(center.x + r * 0.68, y_bot),
                ],
                stroke,
            );
        }
        Icon::Plus => {
            let d = r * 0.62;
            painter.line_segment(
                [Pos2::new(center.x - d, center.y), Pos2::new(center.x + d, center.y)],
                stroke,
            );
            painter.line_segment(
                [Pos2::new(center.x, center.y - d), Pos2::new(center.x, center.y + d)],
                stroke,
            );
        }
        Icon::Cloud => {
            // 清晰云朵轮廓（底部平直 + 三圆弧）
            let y_base = center.y + r * 0.42;
            painter.circle_stroke(Pos2::new(center.x - r * 0.35, center.y + r * 0.08), r * 0.34, thin);
            painter.circle_stroke(Pos2::new(center.x + r * 0.05, center.y - r * 0.12), r * 0.42, thin);
            painter.circle_stroke(Pos2::new(center.x + r * 0.42, center.y + r * 0.12), r * 0.30, thin);
            painter.line_segment(
                [
                    Pos2::new(center.x - r * 0.55, y_base),
                    Pos2::new(center.x + r * 0.58, y_base),
                ],
                stroke,
            );
        }
        Icon::Settings => {
            // 齿轮外圈齿 + 内圈圆
            let inner_r = r * 0.36;
            let outer_r = r * 0.62;
            let tooth_r = r * 0.80;
            painter.circle_stroke(center, inner_r, stroke);
            for i in 0..8 {
                let ang = (i as f32) * std::f32::consts::FRAC_PI_4;
                let (s, c) = ang.sin_cos();
                painter.line_segment(
                    [
                        Pos2::new(center.x + c * outer_r * 0.85, center.y + s * outer_r * 0.85),
                        Pos2::new(center.x + c * tooth_r, center.y + s * tooth_r),
                    ],
                    stroke,
                );
            }
        }
        Icon::Shield => {
            // 盾牌五边形路径
            let top = Pos2::new(center.x, center.y - r * 0.72);
            let tl = Pos2::new(center.x - r * 0.62, center.y - r * 0.42);
            let tr = Pos2::new(center.x + r * 0.62, center.y - r * 0.42);
            let bl = Pos2::new(center.x - r * 0.48, center.y + r * 0.22);
            let br = Pos2::new(center.x + r * 0.48, center.y + r * 0.22);
            let bot = Pos2::new(center.x, center.y + r * 0.76);
            painter.line_segment([top, tl], stroke);
            painter.line_segment([top, tr], stroke);
            painter.line_segment([tl, bl], stroke);
            painter.line_segment([tr, br], stroke);
            painter.line_segment([bl, bot], stroke);
            painter.line_segment([br, bot], stroke);
        }
        Icon::Key => {
            // 钥匙圆环 + 钥匙杆 + 钥匙齿
            let bow = Pos2::new(center.x - r * 0.32, center.y);
            let bow_r = r * 0.36;
            painter.circle_stroke(bow, bow_r, stroke);
            let shaft_start = Pos2::new(bow.x + bow_r, center.y);
            let shaft_end = Pos2::new(center.x + r * 0.72, center.y);
            painter.line_segment([shaft_start, shaft_end], stroke);
            painter.line_segment(
                [
                    Pos2::new(center.x + r * 0.38, center.y),
                    Pos2::new(center.x + r * 0.38, center.y + r * 0.34),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x + r * 0.66, center.y),
                    Pos2::new(center.x + r * 0.66, center.y + r * 0.38),
                ],
                stroke,
            );
        }
        Icon::Lock => {
            // 锁体 + 锁梁
            let body = Rect::from_center_size(
                Pos2::new(center.x, center.y + r * 0.22),
                Vec2::new(size * 0.66, size * 0.50),
            );
            painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Inside);
            let shackle_top = center.y - r * 0.62;
            let sx = size * 0.18;
            painter.line_segment(
                [Pos2::new(center.x - sx, body.min.y), Pos2::new(center.x - sx, shackle_top + 2.0)],
                thin,
            );
            painter.line_segment(
                [Pos2::new(center.x + sx, body.min.y), Pos2::new(center.x + sx, shackle_top + 2.0)],
                thin,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x - sx, shackle_top + 1.5),
                    Pos2::new(center.x + sx, shackle_top + 1.5),
                ],
                thin,
            );
        }
        Icon::Edit => {
            // 2D 立体铅笔轮廓（笔杆平行四边形 + 左下三角笔尖 + 底部基准线）
            let tip = Pos2::new(center.x - r * 0.68, center.y + r * 0.64);
            let b_left = Pos2::new(center.x - r * 0.48, center.y + r * 0.16);
            let b_bottom = Pos2::new(center.x - r * 0.20, center.y + r * 0.44);
            let top_left = Pos2::new(center.x + r * 0.28, center.y - r * 0.60);
            let top_right = Pos2::new(center.x + r * 0.56, center.y - r * 0.32);

            painter.line_segment([tip, b_left], thin);
            painter.line_segment([tip, b_bottom], thin);
            painter.line_segment([b_left, top_left], thin);
            painter.line_segment([b_bottom, top_right], thin);
            painter.line_segment([top_left, top_right], thin);
            painter.line_segment([b_left, b_bottom], thin);
            painter.line_segment(
                [
                    Pos2::new(center.x - r * 0.10, center.y + r * 0.66),
                    Pos2::new(center.x + r * 0.68, center.y + r * 0.66),
                ],
                thin,
            );
        }
        Icon::Search => {
            let c = Pos2::new(center.x - r * 0.12, center.y - r * 0.12);
            let cr = r * 0.48;
            painter.circle_stroke(c, cr, stroke);
            let start = Pos2::new(c.x + cr * 0.7, c.y + cr * 0.7);
            let end = Pos2::new(center.x + r * 0.65, center.y + r * 0.65);
            painter.line_segment([start, end], stroke);
        }
        Icon::Folder => {
            let w = size * 0.78;
            let h = size * 0.56;
            let body = Rect::from_center_size(Pos2::new(center.x, center.y + r * 0.08), Vec2::new(w, h));
            painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Inside);
            painter.line_segment(
                [
                    Pos2::new(body.min.x + 1.0, body.min.y - 2.0),
                    Pos2::new(body.min.x + w * 0.42, body.min.y - 2.0),
                ],
                stroke,
            );
        }
        Icon::Refresh => {
            painter.circle_stroke(center, r * 0.58, stroke);
            painter.circle_filled(Pos2::new(center.x + r * 0.55, center.y - r * 0.18), 2.0, color);
        }
        Icon::Trash => {
            let body = Rect::from_center_size(
                Pos2::new(center.x, center.y + r * 0.12),
                Vec2::new(size * 0.56, size * 0.62),
            );
            painter.rect_stroke(body, CornerRadius::same(2), thin, StrokeKind::Inside);
            let top_y = body.min.y - 2.0;
            painter.line_segment(
                [
                    Pos2::new(center.x - r * 0.65, top_y),
                    Pos2::new(center.x + r * 0.65, top_y),
                ],
                stroke,
            );
        }
        Icon::Copy => {
            let r1 = Rect::from_center_size(
                Pos2::new(center.x - r * 0.14, center.y - r * 0.14),
                Vec2::new(size * 0.52, size * 0.58),
            );
            let r2 = Rect::from_center_size(
                Pos2::new(center.x + r * 0.16, center.y + r * 0.16),
                Vec2::new(size * 0.52, size * 0.58),
            );
            painter.rect_stroke(r1, CornerRadius::same(2), thin, StrokeKind::Inside);
            painter.rect_stroke(r2, CornerRadius::same(2), thin, StrokeKind::Inside);
        }
        Icon::Theme => {
            painter.circle_stroke(center, r * 0.56, stroke);
            painter.line_segment(
                [
                    Pos2::new(center.x, center.y - r * 0.56),
                    Pos2::new(center.x, center.y + r * 0.56),
                ],
                thin,
            );
        }
        Icon::WinMin => {
            painter.line_segment(
                [Pos2::new(center.x - 5.0, center.y), Pos2::new(center.x + 5.0, center.y)],
                Stroke::new(1.2_f32, color),
            );
        }
        Icon::WinMax => {
            let rect = Rect::from_center_size(center, Vec2::new(9.5, 9.5));
            painter.rect_stroke(
                rect,
                CornerRadius::same(1),
                Stroke::new(1.2_f32, color),
                StrokeKind::Inside,
            );
        }
        Icon::WinRestore => {
            let r_back = Rect::from_center_size(Pos2::new(center.x + 1.5, center.y - 1.5), Vec2::new(7.5, 7.5));
            let r_front = Rect::from_center_size(Pos2::new(center.x - 1.5, center.y + 1.5), Vec2::new(7.5, 7.5));
            painter.rect_stroke(
                r_back,
                CornerRadius::same(1),
                Stroke::new(1.1_f32, color),
                StrokeKind::Inside,
            );
            painter.rect_stroke(
                r_front,
                CornerRadius::same(1),
                Stroke::new(1.2_f32, color),
                StrokeKind::Inside,
            );
        }
        Icon::WinClose => {
            let d = 4.5;
            let s = Stroke::new(1.35_f32, color);
            painter.line_segment(
                [Pos2::new(center.x - d, center.y - d), Pos2::new(center.x + d, center.y + d)],
                s,
            );
            painter.line_segment(
                [Pos2::new(center.x + d, center.y - d), Pos2::new(center.x - d, center.y + d)],
                s,
            );
        }
    }
}
