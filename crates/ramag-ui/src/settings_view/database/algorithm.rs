//! ID 转换算法摘要。

use gpui::{Context, ParentElement, Styled, div, prelude::FluentBuilder as _, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};
use ramag_domain::entities::IdConverterKind;

use super::super::SettingsView;

pub(super) fn render_algorithm_summary(
    kind: IdConverterKind,
    custom_alphabet: &str,
    cx: &Context<SettingsView>,
) -> gpui::Div {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let algorithm = id_converter_kind_algorithm(kind);
    let alphabet = if kind.is_custom() {
        Some(custom_alphabet)
    } else {
        id_converter_kind_alphabet(kind)
    };

    v_flex()
        .w_full()
        .p(px(10.0))
        .gap(px(6.0))
        .rounded(px(6.0))
        .bg(theme.secondary)
        .child(div().text_sm().child(format!("算法 · {}", algorithm.name)))
        .child(algorithm_row(
            "@ID -> I",
            algorithm.to_integer,
            theme.accent,
            muted,
        ))
        .child(algorithm_row(
            "@ID -> S",
            algorithm.to_string,
            theme.accent,
            muted,
        ))
        .when_some(alphabet, |summary, alphabet| {
            summary.child(algorithm_row(
                "字符表",
                format!("\"{}\"", quote_contents(alphabet)),
                theme.accent,
                muted,
            ))
        })
}

pub(super) fn quote_contents(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn algorithm_row(
    label: &'static str,
    content: impl Into<gpui::SharedString>,
    label_color: gpui::Hsla,
    text_color: gpui::Hsla,
) -> gpui::Div {
    h_flex()
        .w_full()
        .items_start()
        .gap(px(8.0))
        .text_xs()
        .child(
            div()
                .w(px(58.0))
                .flex_none()
                .text_color(label_color)
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(text_color)
                .child(content.into()),
        )
}

struct IdConverterAlgorithm {
    name: &'static str,
    to_integer: &'static str,
    to_string: &'static str,
}

fn id_converter_kind_algorithm(kind: IdConverterKind) -> IdConverterAlgorithm {
    match kind {
        IdConverterKind::Base10 => IdConverterAlgorithm {
            name: "Base10 位权编码",
            to_integer: "逐字符计算 value = value × 10 + 字符下标",
            to_string: "反复除以 10，按余数查字符表并逆序",
        },
        IdConverterKind::Base16 => IdConverterAlgorithm {
            name: "Base16 位权编码",
            to_integer: "逐字符计算 value = value × 16 + 字符下标；不区分大小写，可带 0x 前缀",
            to_string: "反复除以 16，按余数查字符表并逆序；输出小写且不带前缀",
        },
        IdConverterKind::Base36 => IdConverterAlgorithm {
            name: "Base36 位权编码",
            to_integer: "逐字符计算 value = value × 36 + 字符下标；不区分大小写",
            to_string: "反复除以 36，按余数查字符表并逆序；输出小写",
        },
        IdConverterKind::Base58Bitcoin | IdConverterKind::Base58Flickr => IdConverterAlgorithm {
            name: "Base58 位权编码",
            to_integer: "逐字符计算 value = value × 58 + 字符下标",
            to_string: "反复除以 58，按余数查字符表并逆序",
        },
        IdConverterKind::CustomAlphabet => IdConverterAlgorithm {
            name: "Base-N 位权编码",
            to_integer: "逐字符计算 value = value × N + 字符下标",
            to_string: "反复除以 N，按余数查字符表并逆序",
        },
        IdConverterKind::ExternalProgram => IdConverterAlgorithm {
            name: "外部程序自定义算法",
            to_integer: "执行 <程序> -s <字符串>",
            to_string: "执行 <程序> -i <非负十进制整数>",
        },
    }
}

fn id_converter_kind_alphabet(kind: IdConverterKind) -> Option<&'static str> {
    match kind {
        IdConverterKind::Base10 => Some(ramag_domain::entities::BASE10_ALPHABET),
        IdConverterKind::Base16 => Some(ramag_domain::entities::BASE16_ALPHABET),
        IdConverterKind::Base36 => Some(ramag_domain::entities::BASE36_ALPHABET),
        IdConverterKind::Base58Bitcoin => Some(ramag_domain::entities::BASE58_BITCOIN_ALPHABET),
        IdConverterKind::Base58Flickr => Some(ramag_domain::entities::BASE58_FLICKR_ALPHABET),
        IdConverterKind::CustomAlphabet | IdConverterKind::ExternalProgram => None,
    }
}
