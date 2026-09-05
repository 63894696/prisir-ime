//! 符号 / Emoji 网格面板 — 复刻搜狗「符号大全」(分类 tab + 网格)。
//!
//! **形态**:点状态条「符」/「😀」按钮弹出独立窗,左侧分类 tab(符号面板),
//! 右侧网格,点选符号/emoji 直接上屏。复用候选窗窗口模式
//! (WS_EX_NOACTIVATE+TOPMOST+TOOLWINDOW 自绘),销毁走 WM_CLOSE(跨线程 err=5 教训)。
//!
//! **符号分类(复刻搜狗)**:SYMBOL_CATS 是 (分类名, 符号数组) 列表,
//! 面板顶部画分类 tab,点击切当前分类(cat_idx 存 state),网格只画当前类。
//!
//! **上屏通道**:面板 WndProc 拿不到 TsfInputProcessor,activate 时经
//! `bind_commit` 存 context+client_tid 全局句柄,点选走 edit_session Commit。

use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
// 彩色 emoji: Direct2D/DirectWrite 渲染(COLR)。GDI DrawText 只画单色轮廓,
// 彩色要 ID2D1 + DrawTextLayout(ENABLE_COLOR_FONT)。seguiemj.ttf 系统自带,无需下载字体。
use windows::Win32::Graphics::Direct2D::Common as D2DC;
use windows::Win32::Graphics::Direct2D as D2D;
use windows::Win32::Graphics::DirectWrite as DW;
use windows::Win32::Graphics::Dxgi::Common as DXGI;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    VIRTUAL_KEY,
};

// ── 面板种类 ────────────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PanelKind {
    Symbols,
    Emoji,
}

const U_ARROWS: &[&str] = &[
    "←", "↑", "→", "↓", "↔", "↕", "↖", "↗", "↘", "↙", "↚", "↛", "↜",
    "↝", "↞", "↟", "↠", "↡", "↢", "↣", "↤", "↥", "↦", "↧", "↨", "↩",
    "↪", "↫", "↬", "↭", "↮", "↯", "↰", "↱", "↲", "↳", "↴", "↵", "↶",
    "↷", "↸", "↹", "↺", "↻", "↼", "↽", "↾", "↿", "⇀", "⇁", "⇂", "⇃",
    "⇄", "⇅", "⇆", "⇇", "⇈", "⇉", "⇊", "⇋", "⇌", "⇍", "⇎", "⇏", "⇐",
    "⇑", "⇒", "⇓", "⇔", "⇕", "⇖", "⇗", "⇘", "⇙", "⇚", "⇛", "⇜", "⇝",
    "⇞", "⇟", "⇠", "⇡", "⇢", "⇣", "⇤", "⇥", "⇦", "⇧", "⇨", "⇩", "⇪",
    "⇫", "⇬", "⇭", "⇮", "⇯", "⇰", "⇱", "⇲", "⇳", "⇴", "⇵", "⇶", "⇷",
    "⇸", "⇹", "⇺", "⇻", "⇼", "⇽", "⇾", "⇿", "⟰", "⟱", "⟲", "⟳", "⟴",
    "⟵", "⟶", "⟷", "⟸", "⟹", "⟺", "⟻", "⟼", "⟽", "⟾", "⟿", "⬀", "⬁",
    "⬂", "⬃", "⬄", "⬅", "⬆", "⬇", "⬈", "⬉", "⬊", "⬋", "⬌", "⬍", "⬎",
];

const U_GEOMETRIC: &[&str] = &[
    "■", "□", "▢", "▣", "▤", "▥", "▦", "▧", "▨", "▩", "▪", "▫", "▬",
    "▭", "▮", "▯", "▰", "▱", "▲", "△", "▴", "▵", "▶", "▷", "▸", "▹",
    "►", "▻", "▼", "▽", "▾", "▿", "◀", "◁", "◂", "◃", "◄", "◅", "◆",
    "◇", "◈", "◉", "◊", "○", "◌", "◍", "◎", "●", "◐", "◑", "◒", "◓",
    "◔", "◕", "◖", "◗", "◘", "◙", "◚", "◛", "◜", "◝", "◞", "◟", "◠",
    "◡", "◢", "◣", "◤", "◥", "◦", "◧", "◨", "◩", "◪", "◫", "◬", "◭",
    "◮", "◯", "◰", "◱", "◲", "◳", "◴", "◵", "◶", "◷", "◸", "◹", "◺",
    "◻", "◼", "◽", "◾", "◿",
];

const U_MISCM: &[&str] = &[
    "⌀", "⌁", "⌂", "⌃", "⌄", "⌅", "⌆", "⌇", "⌈", "⌉", "⌊", "⌋", "⌌",
    "⌍", "⌎", "⌏", "⌐", "⌑", "⌒", "⌓", "⌔", "⌕", "⌖", "⌗", "⌘", "⌙",
    "⌚", "⌛", "⌜", "⌝", "⌞", "⌟", "⌠", "⌡", "⌢", "⌣", "⌤", "⌥", "⌦",
    "⌧", "⌨", "〈", "〉", "⌫", "⌬", "⌭", "⌮", "⌯", "⌰", "⌱", "⌲", "⌳",
    "⌴", "⌵", "⌶", "⌷", "⌸", "⌹", "⌺", "⌻", "⌼", "⌽", "⌾", "⌿", "⍀",
    "⍁", "⍂", "⍃", "⍄", "⍅", "⍆", "⍇", "⍈", "⍉", "⍊", "⍋", "⍌", "⍍",
    "⍎", "⍏", "⍐", "⍑", "⍒", "⍓", "⍔", "⍕", "⍖", "⍗", "⍘", "⍙", "⍚",
    "⍛", "⍜", "⍝", "⍞", "⍟", "⍠", "⍡", "⍢", "⍣", "⍤", "⍥", "⍦", "⍧",
    "⍨", "⍩", "⍪", "⍫", "⍬", "⍭", "⍮", "⍯", "⍰", "⍱", "⍲", "⍳", "⍴",
    "⍵", "⍶", "⍷", "⍸", "⍹", "⍺", "⍻", "⍼", "⍽", "⍾", "⍿", "⎀", "⎁",
    "⎂", "⎃", "⎄", "⎅", "⎆", "⎇", "⎈", "⎉", "⎊", "⎋", "⎌", "⎍", "⎎",
    "⎏", "⎐", "⎑", "⎒", "⎓", "⎔", "⎕", "⎖", "⎗", "⎘", "⎙", "⎚", "⎛",
    "⎜", "⎝", "⎞", "⎟", "⎠", "⎡", "⎢", "⎣", "⎤", "⎥", "⎦", "⎧", "⎨",
    "⎩", "⎪", "⎫", "⎬", "⎭", "⎮", "⎯", "⎰", "⎱", "⎲", "⎳", "⎴", "⎵",
    "⎶", "⎷", "⎸", "⎹", "⎺", "⎻", "⎼", "⎽", "⎾", "⎿", "⏀", "⏁", "⏂",
    "⏃", "⏄", "⏅", "⏆", "⏇", "⏈", "⏉", "⏊", "⏋", "⏌", "⏍", "⏎", "⏏",
    "⏐", "⏑", "⏒", "⏓", "⏔", "⏕", "⏖", "⏗", "⏘", "⏙", "⏚", "⏛", "⏜",
    "⏝", "⏞", "⏟", "⏠", "⏡", "⏢", "⏣", "⏤", "⏥", "⏦", "⏧", "⏨", "⏩",
    "⏪", "⏫", "⏬", "⏭", "⏮", "⏯", "⏰", "⏱", "⏲", "⏳", "⏴", "⏵", "⏶",
    "⏷", "⏸", "⏹", "⏺", "⭐", "⭑", "⭒", "⭓",
    "⭔", "⭕",
];

const U_MISCSYM: &[&str] = &[
    "☀", "☁", "☂", "☃", "☄", "★", "☆", "☇", "☈", "☉", "☊", "☋", "☌",
    "☍", "☎", "☏", "☐", "☑", "☒", "☓", "☔", "☕", "☖", "☗", "☘", "☙",
    "☚", "☛", "☜", "☝", "☞", "☟", "☠", "☡", "☢", "☣", "☤", "☥", "☦",
    "☧", "☨", "☩", "☪", "☫", "☬", "☭", "☮", "☯", "☰", "☱", "☲", "☳",
    "☴", "☵", "☶", "☷", "☸", "☹", "☺", "☻", "☼", "☽", "☾", "☿", "♀",
    "♁", "♂", "♃", "♄", "♅", "♆", "♇", "♈", "♉", "♊", "♋", "♌", "♍",
    "♎", "♏", "♐", "♑", "♒", "♓", "♔", "♕", "♖", "♗", "♘", "♙", "♚",
    "♛", "♜", "♝", "♞", "♟", "♠", "♡", "♢", "♣", "♤", "♥", "♦", "♧",
    "♨", "♩", "♪", "♫", "♬", "♭", "♮", "♯", "♰", "♱", "♲", "♳", "♴",
    "♵", "♶", "♷", "♸", "♹", "♺", "♻", "♼", "♽", "♾", "♿", "⚀", "⚁",
    "⚂", "⚃", "⚄", "⚅", "⚆", "⚇", "⚈", "⚉", "⚊", "⚋", "⚌", "⚍", "⚎",
    "⚏", "⚐", "⚑", "⚒", "⚓", "⚔", "⚕", "⚖", "⚗", "⚘", "⚙", "⚚", "⚛",
    "⚜", "⚝", "⚞", "⚟", "⚠", "⚡", "⚢", "⚣", "⚤", "⚥", "⚦", "⚧", "⚨",
    "⚩", "⚪", "⚫", "⚬", "⚭", "⚮", "⚯", "⚰", "⚱", "⚲", "⚳", "⚴", "⚵",
    "⚶", "⚷", "⚸", "⚹", "⚺", "⚻", "⚼", "⚽", "⚾", "⚿", "⛀", "⛁", "⛂",
    "⛃", "⛄", "⛅", "⛆", "⛇", "⛈", "⛉", "⛊", "⛋", "⛌", "⛍", "⛎", "⛏",
    "⛐", "⛑", "⛒", "⛓", "⛔", "⛕", "⛖", "⛗", "⛘", "⛙", "⛚", "⛛", "⛜",
    "⛝", "⛞", "⛟", "⛠", "⛡", "⛢", "⛣", "⛤", "⛥", "⛦", "⛧", "⛨", "⛩",
    "⛪", "⛫", "⛬", "⛭", "⛮", "⛯", "⛰", "⛱", "⛲", "⛳", "⛴", "⛵", "⛶",
    "⛷", "⛸", "⛹", "⛺", "⛻", "⛼", "⛽", "⛾", "⛿",
];

const U_DINGBATS: &[&str] = &[
    "✀", "✁", "✂", "✃", "✄", "✅", "✆", "✇", "✈", "✉", "✊", "✋", "✌",
    "✍", "✎", "✏", "✐", "✑", "✒", "✓", "✔", "✕", "✖", "✗", "✘", "✙",
    "✚", "✛", "✜", "✝", "✞", "✟", "✠", "✡", "✢", "✣", "✤", "✥", "✦",
    "✧", "✨", "✩", "✪", "✫", "✬", "✭", "✮", "✯", "✰", "✱", "✲", "✳",
    "✴", "✵", "✶", "✷", "✸", "✹", "✺", "✻", "✼", "✽", "✾", "✿", "❀",
    "❁", "❂", "❃", "❄", "❅", "❆", "❇", "❈", "❉", "❊", "❋", "❌", "❍",
    "❎", "❏", "❐", "❑", "❒", "❓", "❔", "❕", "❖", "❗", "❘", "❙", "❚",
    "❛", "❜", "❝", "❞", "❟", "❠", "❡", "❢", "❣", "❤", "❥", "❦", "❧",
    "❨", "❩", "❪", "❫", "❬", "❭", "❮", "❯", "❰", "❱", "❲", "❳", "❴",
    "❵", "❶", "❷", "❸", "❹", "❺", "❻", "❼", "❽", "❾", "❿", "➀", "➁",
    "➂", "➃", "➄", "➅", "➆", "➇", "➈", "➉", "➊", "➋", "➌", "➍", "➎",
    "➏", "➐", "➑", "➒", "➓", "➔", "➕", "➖", "➗", "➘", "➙", "➚", "➛",
    "➜", "➝", "➞", "➟", "➠", "➡", "➢", "➣", "➤", "➥", "➦", "➧", "➨",
    "➩", "➪", "➫", "➬", "➭", "➮", "➯", "➰", "➱", "➲", "➳", "➴", "➵",
    "➶", "➷", "➸", "➹", "➺", "➻", "➼", "➽", "➾", "➿",
];

const U_MATH: &[&str] = &[
    "∀", "∁", "∂", "∃", "∄", "∅", "∆", "∇", "∈", "∉", "∊", "∋", "∌",
    "∍", "∎", "∏", "∐", "∑", "−", "∓", "∔", "∕", "∖", "∗", "∘", "∙",
    "√", "∛", "∜", "∝", "∞", "∟", "∠", "∡", "∢", "∣", "∤", "∥", "∦",
    "∧", "∨", "∩", "∪", "∫", "∬", "∭", "∮", "∯", "∰", "∱", "∲", "∳",
    "∴", "∵", "∶", "∷", "∸", "∹", "∺", "∻", "∼", "∽", "∾", "∿", "≀",
    "≁", "≂", "≃", "≄", "≅", "≆", "≇", "≈", "≉", "≊", "≋", "≌", "≍",
    "≎", "≏", "≐", "≑", "≒", "≓", "≔", "≕", "≖", "≗", "≘", "≙", "≚",
    "≛", "≜", "≝", "≞", "≟", "≠", "≡", "≢", "≣", "≤", "≥", "≦", "≧",
    "≨", "≩", "≪", "≫", "≬", "≭", "≮", "≯", "≰", "≱", "≲", "≳", "≴",
    "≵", "≶", "≷", "≸", "≹", "≺", "≻", "≼", "≽", "≾", "≿", "⊀", "⊁",
    "⊂", "⊃", "⊄", "⊅", "⊆", "⊇", "⊈", "⊉", "⊊", "⊋", "⊌", "⊍", "⊎",
    "⊏", "⊐", "⊑", "⊒", "⊓", "⊔", "⊕", "⊖", "⊗", "⊘", "⊙", "⊚", "⊛",
    "⊜", "⊝", "⊞", "⊟", "⊠", "⊡", "⊢", "⊣", "⊤", "⊥", "⊦", "⊧", "⊨",
    "⊩", "⊪", "⊫", "⊬", "⊭", "⊮", "⊯", "⊰", "⊱", "⊲", "⊳", "⊴", "⊵",
    "⊶", "⊷", "⊸", "⊹", "⊺", "⊻", "⊼", "⊽", "⊾", "⊿", "⋀", "⋁", "⋂",
    "⋃", "⋄", "⋅", "⋆", "⋇", "⋈", "⋉", "⋊", "⋋", "⋌", "⋍", "⋎", "⋏",
    "⋐", "⋑", "⋒", "⋓", "⋔", "⋕", "⋖", "⋗", "⋘", "⋙", "⋚", "⋛", "⋜",
    "⋝", "⋞", "⋟", "⋠", "⋡", "⋢", "⋣", "⋤", "⋥", "⋦", "⋧", "⋨", "⋩",
    "⋪", "⋫", "⋬", "⋭", "⋮", "⋯", "⋰", "⋱", "⋲", "⋳", "⋴", "⋵", "⋶",
    "⋷", "⋸", "⋹", "⋺", "⋻", "⋼", "⋽", "⋾", "⋿", "⁰", "ⁱ", "⁴", "⁵", "⁶", "⁷", "⁸", "⁹", "⁺", "⁻", "⁼", "⁽", "⁾", "ⁿ", "₀",
    "₁", "₂", "₃", "₄", "₅", "₆", "₇", "₈", "₉", "₊", "₋", "₌", "₍",
    "₎", "ₐ", "ₑ", "ₒ", "ₓ", "ₔ", "ₕ", "ₖ", "ₗ", "ₘ", "ₙ", "ₚ",
    "ₛ", "ₜ", "℀", "℁", "ℂ", "℃", "℄", "℅", "℆", "ℇ", "℈", "℉", "ℊ",
    "ℋ", "ℌ", "ℍ", "ℎ", "ℏ", "ℐ", "ℑ", "ℒ", "ℓ", "℔", "ℕ", "№", "℗",
    "℘", "ℙ", "ℚ", "ℛ", "ℜ", "ℝ", "℞", "℟", "℠", "℡", "™", "℣", "ℤ",
    "℥", "Ω", "℧", "ℨ", "℩", "K", "Å", "ℬ", "ℭ", "℮", "ℯ", "ℰ", "ℱ",
    "Ⅎ", "ℳ", "ℴ", "ℵ", "ℶ", "ℷ", "ℸ", "ℹ", "℺", "℻", "ℼ", "ℽ", "ℾ",
    "ℿ", "⅀", "⅁", "⅂", "⅃", "⅄", "ⅅ", "ⅆ", "ⅇ", "ⅈ", "ⅉ", "⅊", "⅋",
    "⅌", "⅍", "ⅎ", "⅏",
];

const U_ENCLOSED: &[&str] = &[
    "①", "②", "③", "④", "⑤", "⑥", "⑦", "⑧", "⑨", "⑩", "⑪", "⑫", "⑬",
    "⑭", "⑮", "⑯", "⑰", "⑱", "⑲", "⑳", "⑴", "⑵", "⑶", "⑷", "⑸", "⑹",
    "⑺", "⑻", "⑼", "⑽", "⑾", "⑿", "⒀", "⒁", "⒂", "⒃", "⒄", "⒅", "⒆",
    "⒇", "⒈", "⒉", "⒊", "⒋", "⒌", "⒍", "⒎", "⒏", "⒐", "⒑", "⒒", "⒓",
    "⒔", "⒕", "⒖", "⒗", "⒘", "⒙", "⒚", "⒛", "⒜", "⒝", "⒞", "⒟", "⒠",
    "⒡", "⒢", "⒣", "⒤", "⒥", "⒦", "⒧", "⒨", "⒩", "⒪", "⒫", "⒬", "⒭",
    "⒮", "⒯", "⒰", "⒱", "⒲", "⒳", "⒴", "⒵", "Ⓐ", "Ⓑ", "Ⓒ", "Ⓓ", "Ⓔ",
    "Ⓕ", "Ⓖ", "Ⓗ", "Ⓘ", "Ⓙ", "Ⓚ", "Ⓛ", "Ⓜ", "Ⓝ", "Ⓞ", "Ⓟ", "Ⓠ", "Ⓡ",
    "Ⓢ", "Ⓣ", "Ⓤ", "Ⓥ", "Ⓦ", "Ⓧ", "Ⓨ", "Ⓩ", "ⓐ", "ⓑ", "ⓒ", "ⓓ", "ⓔ",
    "ⓕ", "ⓖ", "ⓗ", "ⓘ", "ⓙ", "ⓚ", "ⓛ", "ⓜ", "ⓝ", "ⓞ", "ⓟ", "ⓠ", "ⓡ",
    "ⓢ", "ⓣ", "ⓤ", "ⓥ", "ⓦ", "ⓧ", "ⓨ", "ⓩ", "⓪", "⓫", "⓬", "⓭", "⓮",
    "⓯", "⓰", "⓱", "⓲", "⓳", "⓴", "⓵", "⓶", "⓷", "⓸", "⓹", "⓺", "⓻",
    "⓼", "⓽", "⓾", "⓿", "⅓", "⅔", "⅕", "⅖", "⅗", "⅘",
    "⅙", "⅚", "⅛", "⅜", "⅝", "⅞", "⅟", "Ⅰ", "Ⅱ", "Ⅲ", "Ⅳ", "Ⅴ", "Ⅵ",
    "Ⅶ", "Ⅷ", "Ⅸ", "Ⅹ", "Ⅺ", "Ⅻ", "Ⅼ", "Ⅽ", "Ⅾ", "Ⅿ", "ⅰ", "ⅱ", "ⅲ",
    "ⅳ", "ⅴ", "ⅵ", "ⅶ", "ⅷ", "ⅸ", "ⅹ", "ⅺ", "ⅻ", "ⅼ", "ⅽ", "ⅾ", "ⅿ",
    "ↀ", "ↁ", "ↂ", "Ↄ", "ↄ", "ↅ", "ↆ", "ↇ", "ↈ", "㈠", "㈡", "㈢", "㈣", "㈤", "㈥", "㈦", "㈧", "㈨", "㈩",
    "㈪", "㈫", "㈬", "㈭", "㈮", "㈯", "㈰", "㈱", "㈳", "㈴", "㈵", "㈶",
    "㈷", "㈸", "㈺", "㈻", "㈼", "㈽", "㈾", "㈿", "㉀", "㉁", "㉂", "㉃",
    "㉄", "㉅", "㉆", "㉇", "㉈", "㉉", "㉊", "㉋", "㉌", "㉍", "㉎", "㉏", "㉐",
    "㉑", "㉒", "㉓", "㉔", "㉕", "㉖", "㉗", "㉘", "㉙", "㉚", "㉛", "㉜", "㉝",
    "㉞", "㉟", "㊀", "㊁", "㊂", "㊃", "㊄", "㊅", "㊆", "㊇", "㊈", "㊉", "㊊",
    "㊋", "㊌", "㊍", "㊎", "㊏", "㊐", "㊑", "㊒", "㊓", "㊔", "㊕", "㊖", "㊗",
    "㊘", "㊙", "㊚", "㊛", "㊜", "㊝", "㊞", "㊟", "㊠", "㊡", "㊢", "㊣", "㊩", "㊪", "㊫", "㊬", "㊭", "㊮", "㊯", "㊰", "㊱",
    "㊲", "㊳", "㊴", "㊵", "㊶", "㊷", "㊸", "㊹", "㊺", "㊻", "㊼", "㊽", "㊾",
    "㊿",
];

const U_CJKPUNCT: &[&str] = &[
    "　", "、", "。", "〃", "〄", "々", "〆", "〇", "〈", "〉", "《", "》", "「",
    "」", "『", "』", "【", "】", "〒", "〓", "〔", "〕", "〖", "〗", "〘", "〙",
    "〚", "〛", "〜", "〝", "〞", "〟", "〠", "〡", "〢", "〣", "〤", "〥", "〦",
    "〧", "〨", "〩", "〪", "〫", "〬", "〭", "〮", "〯", "〰", "〱", "〲", "〳",
    "〴", "〵", "〶", "〷", "〸", "〹", "〺", "〻", "〼", "〽", "〾", "〿", "︰",
    "︱", "︲", "︳", "︴", "︵", "︶", "︷", "︸", "︹", "︺", "︻", "︼", "︽",
    "︾", "︿", "﹀", "﹁", "﹂", "﹃", "﹄", "﹅", "﹆", "﹇", "﹈", "﹉", "﹊",
    "﹋", "﹌", "﹍", "﹎", "﹏", "﹐", "﹑", "﹒", "﹔", "﹕", "﹖", "﹗",
    "﹘", "﹙", "﹚", "﹛", "﹜", "﹝", "﹞", "﹟", "﹠", "﹡", "﹢", "﹣", "﹤",
    "﹥", "﹦", "﹨", "﹩", "﹪", "﹫",
];

const U_FULLHALF: &[&str] = &[
    "！", "＂", "＃", "＄", "％", "＆", "＇", "（", "）", "＊", "＋", "，", "－",
    "．", "／", "０", "１", "２", "３", "４", "５", "６", "７", "８", "９", "：",
    "；", "＜", "＝", "＞", "？", "＠", "［", "＼", "］", "＾", "＿", "｀", "｛",
    "｜", "｝", "～", "｟", "｠", "｡", "｢", "｣", "､", "･",
];

const U_GENPUNCT: &[&str] = &[
    "‐", "‑", "‒", "–", "—", "―", "‖", "‗", "‘", "’", "‚", "‛", "“",
    "”", "„", "‟", "†", "‡", "•", "‣", "․", "‥", "…", "‧", "‰", "‱",
    "′", "″", "‴", "‵", "‶", "‷", "‸", "‹", "›", "※", "‼", "‽", "‾",
    "‿", "⁀", "⁁", "⁂", "⁃", "⁄", "⁅", "⁆", "⁇", "⁈", "⁉", "⁊", "⁋",
    "⁌", "⁍", "⁎", "⁏", "⁐", "⁑", "⁒", "⁓", "⁔", "⁕", "⁖", "⁗", "⁘",
    "⁙", "⁚", "⁛", "⁜", "⁝", "⁞",
];

const U_GREEK: &[&str] = &[
    "Α", "Β", "Γ", "Δ", "Ε", "Ζ", "Η", "Θ", "Ι", "Κ", "Λ", "Μ", "Ν",
    "Ξ", "Ο", "Π", "Ρ", "Σ", "Τ", "Υ", "Φ", "Χ", "Ψ", "Ω", "Ϊ",
    "Ϋ", "ά", "έ", "ή", "ί", "ΰ", "α", "β", "γ", "δ", "ε", "ζ", "η",
    "θ", "ι", "κ", "λ", "μ", "ν", "ξ", "ο", "π", "ρ", "ς", "σ", "τ",
    "υ", "φ", "χ", "ψ", "ω", ];

const U_LATIN: &[&str] = &[
    "Ā", "ā", "Ă", "ă", "Ą", "ą", "Ć", "ć", "Ĉ", "ĉ", "Ċ", "ċ", "Č",
    "č", "Ď", "ď", "Đ", "đ", "Ē", "ē", "Ĕ", "ĕ", "Ė", "ė", "Ę", "ę",
    "Ě", "ě", "Ĝ", "ĝ", "Ğ", "ğ", "Ġ", "ġ", "Ģ", "ģ", "Ĥ", "ĥ", "Ħ",
    "ħ", "Ī", "ī", "Ĭ", "ĭ", "Į", "į", "İ", "ı", "Ĳ", "ĳ",
    "Ĵ", "ĵ", "Ķ", "ķ", "ĸ", "Ĺ", "ĺ", "ļ", "Ľ", "ľ", "Ŀ", "ŀ",
    "Ł", "ł", "Ń", "ń", "Ņ", "ņ", "Ň", "ň", "ŉ", "Ŋ", "ŋ", "Ō", "ō",
    "Ŏ", "ŏ", "Ő", "ő", "Œ", "œ", "Ŕ", "ŕ", "Ŗ", "ŗ", "Ř", "ř", "Ś",
    "ś", "Ŝ", "ŝ", "Ş", "ş", "Š", "š", "Ţ", "ţ", "Ť", "ť", "Ŧ", "ŧ",
    "Ũ", "ũ", "Ū", "ū", "Ŭ", "ŭ", "Ů", "ů", "Ű", "ű", "Ų", "ų", "Ŵ",
    "ŵ", "Ŷ", "ŷ", "Ÿ", "Ź", "ź", "Ż", "ż", "Ž", "ž", "ſ", "ƒ", "Ǎ", "ǎ", "Ǐ",
    "ǐ", "Ǒ", "ǒ", "Ǔ", "ǔ", "Ǖ", "ǖ", "Ǘ", "ǘ", "Ǚ", "ǚ", "Ǜ", "ǜ",
    "Ǹ", "ǹ", "Ǻ", "ǻ", "Ǽ", "ǽ", "Ǿ", "ǿ", "Ș", "ș", "Ț", "ț", ];

const U_BOPOMOFO: &[&str] = &[
    "ㄅ", "ㄆ", "ㄇ", "ㄈ", "ㄉ", "ㄊ", "ㄋ", "ㄌ", "ㄍ", "ㄎ", "ㄏ", "ㄐ", "ㄑ",
    "ㄒ", "ㄓ", "ㄔ", "ㄕ", "ㄖ", "ㄗ", "ㄘ", "ㄙ", "ㄚ", "ㄛ", "ㄜ", "ㄝ", "ㄞ",
    "ㄟ", "ㄠ", "ㄡ", "ㄢ", "ㄣ", "ㄤ", "ㄥ", "ㄦ", "ㄧ", "ㄨ", "ㄩ", "ㄪ", "ㄫ",
    "ㄬ", "ㄭ", "ㆠ", "ㆡ", "ㆢ", "ㆣ", "ㆤ", "ㆥ", "ㆦ", "ㆧ", "ㆨ",
    "ㆩ", "ㆪ", "ㆫ", "ㆬ", "ㆭ", "ㆮ", "ㆯ", "ㆰ", "ㆱ", "ㆲ", "ㆳ", "ㆴ", "ㆵ",
    "ㆶ", "ㆷ", ];

const U_IPA: &[&str] = &[
    "ɑ", "ɡ", "ʤ", "ˆ", "ˇ", "ˉ", "ˊ", "ˋ", "ˍ", "˘", "˙", "˚", "˛", "˜", "˝", "˪", "˫",
    ];

const U_HIRAGANA: &[&str] = &[
    "ぁ", "あ", "ぃ", "い", "ぅ", "う", "ぇ", "え", "ぉ", "お", "か", "が", "き",
    "ぎ", "く", "ぐ", "け", "げ", "こ", "ご", "さ", "ざ", "し", "じ", "す", "ず",
    "せ", "ぜ", "そ", "ぞ", "た", "だ", "ち", "ぢ", "っ", "つ", "づ", "て", "で",
    "と", "ど", "な", "に", "ぬ", "ね", "の", "は", "ば", "ぱ", "ひ", "び", "ぴ",
    "ふ", "ぶ", "ぷ", "へ", "べ", "ぺ", "ほ", "ぼ", "ぽ", "ま", "み", "む", "め",
    "も", "ゃ", "や", "ゅ", "ゆ", "ょ", "よ", "ら", "り", "る", "れ", "ろ", "ゎ",
    "わ", "ゐ", "ゑ", "を", "ん", "ゔ", "ゕ", "ゖ",
];

const U_KATAKANA: &[&str] = &[
    "ァ", "ア", "ィ", "イ", "ゥ", "ウ", "ェ", "エ", "ォ", "オ", "カ", "ガ", "キ",
    "ギ", "ク", "グ", "ケ", "ゲ", "コ", "ゴ", "サ", "ザ", "シ", "ジ", "ス", "ズ",
    "セ", "ゼ", "ソ", "ゾ", "タ", "ダ", "チ", "ヂ", "ッ", "ツ", "ヅ", "テ", "デ",
    "ト", "ド", "ナ", "ニ", "ヌ", "ネ", "ノ", "ハ", "バ", "パ", "ヒ", "ビ", "ピ",
    "フ", "ブ", "プ", "ヘ", "ベ", "ペ", "ホ", "ボ", "ポ", "マ", "ミ", "ム", "メ",
    "モ", "ャ", "ヤ", "ュ", "ユ", "ョ", "ヨ", "ラ", "リ", "ル", "レ", "ロ", "ヮ",
    "ワ", "ヰ", "ヱ", "ヲ", "ン", "ヴ", "ヵ", "ヶ", "ヷ", "ヸ", "ヹ", "ヺ", "ヽ",
    "ヾ", "ヿ", "ｦ", "ｧ", "ｨ", "ｩ", "ｪ", "ｫ", "ｬ", "ｭ", "ｮ", "ｯ", "ｰ",
    "ｱ", "ｲ", "ｳ", "ｴ", "ｵ", "ｶ", "ｷ", "ｸ", "ｹ", "ｺ", "ｻ", "ｼ", "ｽ",
    "ｾ", "ｿ", "ﾀ", "ﾁ", "ﾂ", "ﾃ", "ﾄ", "ﾅ", "ﾆ", "ﾇ", "ﾈ", "ﾉ", "ﾊ",
    "ﾋ", "ﾌ", "ﾍ", "ﾎ", "ﾏ", "ﾐ", "ﾑ", "ﾒ", "ﾓ", "ﾔ", "ﾕ", "ﾖ", "ﾗ",
    "ﾘ", "ﾙ", "ﾚ", "ﾛ", "ﾜ", "ﾝ",
];

const U_CYRILLIC: &[&str] = &[
    "Ё", "А", "Б", "В", "Г", "Д", "Е", "Ж", "З", "И", "Й", "К", "Л",
    "М", "Н", "О", "П", "Р", "С", "Т", "У", "Ф", "Х", "Ц", "Ч", "Ш",
    "Щ", "Ъ", "Ы", "Ь", "Э", "Ю", "Я", "а", "б", "в", "г", "д", "е",
    "ж", "з", "и", "й", "к", "л", "м", "н", "о", "п", "р", "с", "т",
    "у", "ф", "х", "ц", "ч", "ш", "щ", "ъ", "ы", "ь", "э", "ю", "я",
    "ё",
];

const U_BOXDRAW: &[&str] = &[
    "─", "━", "│", "┃", "┄", "┅", "┆", "┇", "┈", "┉", "┊", "┋", "┌",
    "┍", "┎", "┏", "┐", "┑", "┒", "┓", "└", "┕", "┖", "┗", "┘", "┙",
    "┚", "┛", "├", "┝", "┞", "┟", "┠", "┡", "┢", "┣", "┤", "┥", "┦",
    "┧", "┨", "┩", "┪", "┫", "┬", "┭", "┮", "┯", "┰", "┱", "┲", "┳",
    "┴", "┵", "┶", "┷", "┸", "┹", "┺", "┻", "┼", "┽", "┾", "┿", "╀",
    "╁", "╂", "╃", "╄", "╅", "╆", "╇", "╈", "╉", "╊", "╋", "╌", "╍",
    "╎", "╏", "═", "║", "╒", "╓", "╔", "╕", "╖", "╗", "╘", "╙", "╚",
    "╛", "╜", "╝", "╞", "╟", "╠", "╡", "╢", "╣", "╤", "╥", "╦", "╧",
    "╨", "╩", "╪", "╫", "╬", "╭", "╮", "╯", "╰", "╱", "╲", "╳", "╴",
    "╵", "╶", "╷", "╸", "╹", "╺", "╻", "╼", "╽", "╾", "╿", "▀", "▁",
    "▂", "▃", "▄", "▅", "▆", "▇", "█", "▉", "▊", "▋", "▌", "▍", "▎",
    "▏", "▐", "░", "▒", "▓", "▔", "▕", "▖", "▗", "▘", "▙", "▚", "▛",
    "▜", "▝", "▞", "▟",
];

const SYMBOL_CATS: &[(&str, &[&str])] = &[
    ("箭头", U_ARROWS),
    ("几何图形", U_GEOMETRIC),
    ("杂项技术", U_MISCM),
    ("杂项符号", U_MISCSYM),
    ("装饰符号", U_DINGBATS),
    ("数学运算符", U_MATH),
    ("带圈/序号", U_ENCLOSED),
    ("中日韩标点", U_CJKPUNCT),
    ("全角/半角", U_FULLHALF),
    ("通用标点", U_GENPUNCT),
    ("希腊/音标", U_GREEK),
    ("拉丁扩展", U_LATIN),
    ("注音符号", U_BOPOMOFO),
    ("国际音标", U_IPA),
    ("平假名", U_HIRAGANA),
    ("片假名", U_KATAKANA),
    ("西里尔字母", U_CYRILLIC),
    ("制表/方块", U_BOXDRAW),
];

const EMOJI_SMILE: &[&str] = &[
    "😀", "😁", "😂", "🤣", "😃", "😄", "😅", "😆",
    "😉", "😊", "😋", "😎", "😍", "😘", "😗", "😙",
    "😚", "☺", "🙂", "🤗", "🤩", "🤔", "🤨", "😐",
    "😑", "😶", "🙄", "😏", "😣", "😥", "😮", "🤐",
    "😯", "😪", "😫", "😴", "😌", "😛", "😜", "😝",
    "🤤", "😒", "😓", "😔", "😕", "🙃", "🤑", "😲",
];

const EMOJI_FACE: &[&str] = &[
    "🥺", "😬", "😖", "😣", "😢", "😭", "😤", "😠",
    "😡", "🤬", "🤯", "😳", "🥵", "🥶", "😱", "😨",
    "😰", "😥", "😓", "😩", "😫", "🥱", "😦", "😧",
    "😸", "😹", "😺", "😻", "😼", "😽", "😾", "😿",
    "🙀", "🤧", "🤪", "🤫", "🤭", "🧐", "🤥",
];

const EMOJI_HAND: &[&str] = &[
    "👋", "🤚", "🖐", "✋", "🖖", "👌",
    "✌", "🤞", "🤟", "🤘", "🤙", "👈", "👉", "👆",
    "🖕", "👇", "☝", "👍", "👎", "✊", "👊", "🤛",
    "🤜", "👏", "🙌", "👐", "🤲", "🤝", "🙏", "✍",
    "💅", "🤳", "💪", "🦾", "🦿", "🦵", "🦶",
];

const EMOJI_HEART: &[&str] = &[
    "❤", "🧡", "💛", "💚", "💙", "💜", "🤎", "🖤",
    "🤍", "💔", "❣", "💕", "💞", "💓", "💗", "💖",
    "💘", "💝", "💟", "💌", "💋", "💍", "💎", "👑",
    "👒", "🎩", "🎓", "📦", "🎁", "🎉", "🎈", "🎀",
    "🎂", "🎆", "🎇", "🎐", "🎑", "🧧", "🎖",
];

const EMOJI_ANIMAL: &[&str] = &[
    "🐶", "🐱", "🐭", "🐹", "🐰", "🦊", "🐻", "🐼",
    "🐨", "🐯", "🦁", "🐮", "🐷", "🐽", "🐸", "🐵",
    "🐒", "🐔", "🐧", "🐦", "🐤", "🐣", "🐥", "🦆",
    "🦅", "🦉", "🦇", "🐺", "🐗", "🐴", "🦄", "🐝",
    "🐛", "🐌", "🐞", "🐟", "🐡", "🐠", "🐚", "🐙",
];

const EMOJI_FOOD: &[&str] = &[
    "🍎", "🍊", "🍋", "🍌", "🍉", "🍇", "🍓", "🍈",
    "🍒", "🍑", "🍍", "🍅", "🍆", "🌶", "🌽", "🍠",
    "🍔", "🍟", "🍕", "🌭", "🌮", "🍜", "🍝", "🍛",
    "🍙", "🍗", "🍖", "🍡", "🍢", "🍣", "🍤", "🍥",
    "🍦", "🍧", "🍨", "🍩", "🍪", "🍫", "🍬",
];

const EMOJI_TRANSPORT: &[&str] = &[
    "🚗", "🚕", "🚙", "🚌", "🚎", "🏎", "🚓", "🚑",
    "🚒", "🚐", "🚚", "🚛", "🚜", "🛵", "🏍", "🚲",
    "🛴", "🛹", "🚨", "🚔", "🚍", "🚘", "🚖", "🚡",
    "🚠", "🚟", "🚃", "🚋", "🚞", "🚝", "🚄", "🚅",
    "🚈", "🚂", "🚆", "🚇", "🚊", "🚉", "✈", "🛫",
];

const EMOJI_OBJECT: &[&str] = &[
    "⌚", "📱", "📲", "💻", "⌨", "🖥", "🖨", "🖱",
    "🖲", "🕹", "🗜", "💽", "💾", "💿", "📀", "📚",
    "📖", "📗", "📘", "📙", "📓", "📔", "📒", "📕",
    "📝", "📄", "📰", "📞", "📟", "📠", "☎", "📧",
    "📨", "📩", "📪", "📫", "📬", "📭", "📮",
];

const EMOJI_SYMBOL: &[&str] = &[
    "❤", "🚩", "🏁", "🚩", "🎌", "🏴", "🏳", "🏳️‍🌈",
    "🏴‍☠️", "🏆", "🏅", "🥇", "🥈", "🥉", "🎵", "🎶",
    "🎤", "🎧", "🎷", "🎸", "🎹", "🎺", "🎻", "🎼",
    "🎭", "🎨", "🎬", "🎥", "🎮", "🎲", "🎯", "🎳",
    "🎰", "🎱", "🃏", "🀄", "🎴", "♟", "🏀", "🏈",
];

const EMOJI_WEATHER: &[&str] = &[
    "☀", "🌤", "⛅", "🌥", "☁", "🌦", "🌧", "⛈",
    "🌨", "🌩", "🌪", "🌫", "🌬", "🌀", "🌈", "🌂",
    "☂", "☔", "⛱", "⚡", "❄", "☃", "⛄", "☄",
    "🌑", "🌒", "🌓", "🌔", "🌕", "🌖", "🌗", "🌘",
    "🌙", "🌚", "🌛", "🌜", "🌝", "🌞", "⭐", "🌟",
    "✨",
];
const EMOJI_CATS: &[(&str, &[&str])] = &[
    ("笑脸", EMOJI_SMILE),
    ("表情", EMOJI_FACE),
    ("手势", EMOJI_HAND),
    ("爱心", EMOJI_HEART),
    ("动物", EMOJI_ANIMAL),
    ("食物", EMOJI_FOOD),
    ("交通", EMOJI_TRANSPORT),
    ("物品", EMOJI_OBJECT),
    ("符号", EMOJI_SYMBOL),
    ("天气", EMOJI_WEATHER),
];


// ── 网格布局常量(左侧分类列表 + 右侧网格,对齐搜狗)─────────────────────────
const COLS: i32 = 13;          // 网格每行格数(对齐搜狗截图)
const CELL: i32 = 30;          // 每格边长(px)
const PAD: i32 = 6;            // 面板内边距
const CAT_W: i32 = 104;        // 左侧分类列表宽(加 2 字宽,容纳最长 5 字分类名)
const CAT_ROW_H: i32 = 24;     // 分类列表行高
const MAX_ROWS: i32 = 15;      // 网格可视行数(分类 19 行,网格给 15 行,超出滚轮)

fn panel_width() -> i32 { PAD * 2 + CAT_W + COLS * CELL }
fn panel_height(n_items: usize, kind: PanelKind) -> i32 {
    let grid_rows = (((n_items as i32) + COLS - 1) / COLS).min(MAX_ROWS);
    let grid_h = grid_rows * CELL;
    // 分类列表高度(符号/emoji 都有分类)可能超过网格,取大者。
    let cat_h = (cats_for(kind).len() as i32) * CAT_ROW_H;
    PAD * 2 + grid_h.max(cat_h)
}

/// 建 UI 字体(默认 Microsoft YaHei,所有界面文字统一用雅黑)。
/// 返回 (font, old_font_in_hdc) 供用后还原 + DeleteObject;失败返回 None。
unsafe fn make_ui_font(hdc: HDC, size: i32, face: &str) -> Option<(HFONT, HGDIOBJ)> {
    let face: Vec<u16> = face.encode_utf16().chain(std::iter::once(0)).collect();
    let f = CreateFontW(
        -size, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0,
        DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32, DEFAULT_PITCH.0 as u32, PCWSTR(face.as_ptr()),
    );
    if !f.is_invalid() {
        let old = SelectObject(hdc, f);
        Some((f, old))
    } else { None }
}

// ── 共享状态 ────────────────────────────────────────────────────────────────
pub(crate) struct PanelState {
    pub hwnd: Option<HWND>,
    pub kind: PanelKind,
    pub cat_idx: usize,   // 符号面板当前分类
    pub scroll_row: i32,  // 网格滚动到的起始行(滚轮)
    pub visible: bool,
    pub x: i32,
    pub y: i32,
}

impl PanelState {
    pub fn new() -> Self {
        Self {
            hwnd: None,
            kind: PanelKind::Symbols,
            cat_idx: 0,
            scroll_row: 0,
            visible: false,
            x: 100,
            y: 100,
        }
    }
}

static PANEL_STATE: OnceLock<Arc<Mutex<RefCell<PanelState>>>> = OnceLock::new();
unsafe impl Send for PanelState {}
unsafe impl Sync for PanelState {}

pub(crate) fn global_panel_state() -> Arc<Mutex<RefCell<PanelState>>> {
    PANEL_STATE
        .get_or_init(|| Arc::new(Mutex::new(RefCell::new(PanelState::new()))))
        .clone()
}

// ── DirectWrite 彩色 emoji 资源缓存 ─────────────────────────────────────────
// GDI DrawText 只画 emoji 单色轮廓;彩色(COLR)要 D2D + DirectWrite。
// 工厂/渲染目标/画刷/字体格式建一次缓存复用;设备丢失(EndDraw 返回 D2DERR_RECREATE_TARGET)
// 时置 None 下次重建。单 STA 线程,RefCell 无真并发。hwnd 存一份以便校验尺寸变化。
struct D2dRes {
    hwnd: HWND,
    hwnd_rt: D2D::ID2D1HwndRenderTarget,
    rt: D2D::ID2D1RenderTarget,
    brush: D2D::ID2D1SolidColorBrush,
    grid_brush: D2D::ID2D1SolidColorBrush,
    text_brush: D2D::ID2D1SolidColorBrush,     // 分类名(普通,深灰)
    sel_brush: D2D::ID2D1SolidColorBrush,      // 分类名(选中,橙)
    selbg_brush: D2D::ID2D1SolidColorBrush,    // 分类选中底色(浅)
    dwrite: DW::IDWriteFactory,
    fmt: DW::IDWriteTextFormat,      // emoji 字形 18px
    cat_fmt: DW::IDWriteTextFormat,  // 分类名 16px 雅黑
}
thread_local! {
    static D2D_RES: RefCell<Option<D2dRes>> = RefCell::new(None);
}

/// 确保 D2D 资源就绪(rt 绑定当前 hwnd)。返 false = 建失败,回退 GDI 单色。
fn ensure_d2d(hwnd: HWND) -> bool {
    D2D_RES.with(|cell| {
        let mut slot = cell.borrow_mut();
        if let Some(r) = &*slot {
            // hwnd 换了(面板重建)就丢弃旧的重建。
            if r.hwnd == hwnd { return true; }
        }
        *slot = build_d2d(hwnd);
        slot.is_some()
    })
}

fn build_d2d(hwnd: HWND) -> Option<D2dRes> {
    unsafe {
        let factory: D2D::ID2D1Factory =
            D2D::D2D1CreateFactory(D2D::D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ok()?;
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let rt_props = D2D::D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D::D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2DC::D2D1_PIXEL_FORMAT {
                format: DXGI::DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2DC::D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: 0.0,
            dpiY: 0.0,
            usage: D2D::D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D::D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hwnd_props = D2D::D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: D2DC::D2D_SIZE_U {
                width: (rc.right - rc.left).max(1) as u32,
                height: (rc.bottom - rc.top).max(1) as u32,
            },
            presentOptions: D2D::D2D1_PRESENT_OPTIONS_NONE,
        };
        let rt0 = factory.CreateHwndRenderTarget(&rt_props, &hwnd_props).ok()?;
        // HwndRenderTarget 上不直接暴露 RenderTarget 方法,cast 到 ID2D1RenderTarget。
        let rt: D2D::ID2D1RenderTarget = rt0.cast().ok()?;
        // 画刷颜色无关(COLR 彩字形忽略 fill),给个不透明黑占位。
        let black = D2DC::D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
        let brush = rt.CreateSolidColorBrush(&black, None).ok()?;
        // 网格线画刷(浅灰,对齐 GDI 0xDDDDDD)。
        let grey = D2DC::D2D1_COLOR_F { r: 0.866, g: 0.866, b: 0.866, a: 1.0 };
        let grid_brush = rt.CreateSolidColorBrush(&grey, None).ok()?;
        // 分类名画刷:普通深灰(0x333333,白底清晰)/ 选中橙(0x2A7FC4)。
        let text_c = D2DC::D2D1_COLOR_F { r: 0.2, g: 0.2, b: 0.2, a: 1.0 };
        let text_brush = rt.CreateSolidColorBrush(&text_c, None).ok()?;
        let sel_c = D2DC::D2D1_COLOR_F { r: 0.769, g: 0.498, b: 0.165, a: 1.0 }; // 0xC47F2A
        let sel_brush = rt.CreateSolidColorBrush(&sel_c, None).ok()?;
        let selbg_c = D2DC::D2D1_COLOR_F { r: 0.957, g: 0.894, b: 0.831, a: 1.0 }; // 浅橙底
        let selbg_brush = rt.CreateSolidColorBrush(&selbg_c, None).ok()?;
        let dwrite: DW::IDWriteFactory =
            DW::DWriteCreateFactory(DW::DWRITE_FACTORY_TYPE_SHARED).ok()?;
        let locale: Vec<u16> = "zh-cn".encode_utf16().chain(std::iter::once(0)).collect();
        // emoji 字形格式(18px Segoe UI Emoji)。
        let family: Vec<u16> = "Segoe UI Emoji".encode_utf16().chain(std::iter::once(0)).collect();
        let fmt = dwrite.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None,
            DW::DWRITE_FONT_WEIGHT_NORMAL,
            DW::DWRITE_FONT_STYLE_NORMAL,
            DW::DWRITE_FONT_STRETCH_NORMAL,
            18.0,
            PCWSTR(locale.as_ptr()),
        ).ok()?;
        let _ = fmt.SetTextAlignment(DW::DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = fmt.SetParagraphAlignment(DW::DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        // 分类名格式(16px 雅黑,居中)。
        let cat_family: Vec<u16> = "Microsoft YaHei".encode_utf16().chain(std::iter::once(0)).collect();
        let cat_fmt = dwrite.CreateTextFormat(
            PCWSTR(cat_family.as_ptr()),
            None,
            DW::DWRITE_FONT_WEIGHT_NORMAL,
            DW::DWRITE_FONT_STYLE_NORMAL,
            DW::DWRITE_FONT_STRETCH_NORMAL,
            16.0,
            PCWSTR(locale.as_ptr()),
        ).ok()?;
        let _ = cat_fmt.SetTextAlignment(DW::DWRITE_TEXT_ALIGNMENT_CENTER);
        let _ = cat_fmt.SetParagraphAlignment(DW::DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        Some(D2dRes { hwnd, hwnd_rt: rt0, rt, brush, grid_brush, text_brush, sel_brush, selbg_brush, dwrite, fmt, cat_fmt })
    }
}

/// 用 DirectWrite 画整个 emoji 面板(背景+分类列表+网格线+彩色字形),一次
/// BeginDraw/EndDraw 完成,避免 GDI/D2D 同 hwnd 混画导致的整窗覆盖/分类文字被刷黑。
/// 设备丢失则丢弃资源返回 false,外层下帧重建。分类文字直接 DWrite 画(雅黑)。
fn d2d_draw_emoji_grid(hwnd: HWND, items: &[&str], scroll_row: i32, cat_idx: usize) -> bool {
    let drawn = D2D_RES.with(|cell| {
        let slot = cell.borrow();
        let Some(res) = &*slot else { return false; };
        unsafe {
            // 尺寸若变(分类切换重排)同步 rt。
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let w = (rc.right - rc.left).max(1) as u32;
            let h = (rc.bottom - rc.top).max(1) as u32;
            let _ = res.hwnd_rt.Resize(&D2DC::D2D_SIZE_U { width: w, height: h });

            res.rt.BeginDraw();
            // 整窗透明白底。
            let white = D2DC::D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
            res.rt.Clear(Some(&white));

            // ── 左侧分类列表 ──
            let cats = cats_for(PanelKind::Emoji);
            for (i, (name, _)) in cats.iter().enumerate() {
                let selected = i == cat_idx;
                let top = (PAD + (i as i32) * CAT_ROW_H) as f32;
                let rect = D2DC::D2D_RECT_F {
                    left: PAD as f32, top,
                    right: (PAD + CAT_W) as f32, bottom: top + CAT_ROW_H as f32,
                };
                if selected {
                    res.rt.FillRectangle(&rect, &res.selbg_brush);
                }
                let brush: &D2D::ID2D1SolidColorBrush =
                    if selected { &res.sel_brush } else { &res.text_brush };
                let wide: Vec<u16> = name.encode_utf16().collect();
                res.rt.DrawText(
                    &wide, &res.cat_fmt, &rect, brush,
                    D2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DW::DWRITE_MEASURING_MODE_NATURAL,
                );
            }

            // ── 右侧网格区 ──
            let gx = (PAD + CAT_W) as f32;
            let grid_w = (COLS * CELL) as f32;
            let skip = (scroll_row * COLS) as usize;
            let total_rows = ((items.len() as i32) + COLS - 1) / COLS;
            let vis_rows = (total_rows - scroll_row).min(MAX_ROWS);
            let gh = (vis_rows * CELL) as f32;
            // 网格线。
            for r in 0..=vis_rows {
                let y = (PAD + r * CELL) as f32;
                res.rt.DrawLine(
                    D2DC::D2D_POINT_2F { x: gx, y },
                    D2DC::D2D_POINT_2F { x: gx + grid_w, y },
                    &res.grid_brush, 1.0, None,
                );
            }
            for c in 0..=COLS {
                let x = gx + (c * CELL) as f32;
                res.rt.DrawLine(
                    D2DC::D2D_POINT_2F { x, y: PAD as f32 },
                    D2DC::D2D_POINT_2F { x, y: PAD as f32 + gh },
                    &res.grid_brush, 1.0, None,
                );
            }
            // 彩色 emoji 字形。
            for (i, it) in items.iter().enumerate().skip(skip) {
                let vis = i - skip;
                let col = (vis as i32) % COLS;
                let row = (vis as i32) / COLS;
                if row >= MAX_ROWS { break; }
                let cx = gx + (col * CELL) as f32;
                let cy = (PAD + row * CELL) as f32;
                let wide: Vec<u16> = it.encode_utf16().collect();
                let Ok(layout) = res.dwrite.CreateTextLayout(&wide, &res.fmt, CELL as f32, CELL as f32)
                else { continue; };
                res.rt.DrawTextLayout(
                    D2DC::D2D_POINT_2F { x: cx, y: cy },
                    &layout,
                    &res.brush,
                    D2D::D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                );
            }

            let mut device_lost = false;
            if let Err(e) = res.rt.EndDraw(None, None) {
                if e.code() == D2DERR_RECREATE_TARGET {
                    device_lost = true;
                }
            }
            !device_lost
        }
    });
    if !drawn {
        D2D_RES.with(|cell| { *cell.borrow_mut() = None; });
    }
    drawn
}

/// 当前面板用的分类表(符号/emoji 都有分类)。
fn cats_for(kind: PanelKind) -> &'static [(&'static str, &'static [&'static str])] {
    match kind {
        PanelKind::Symbols => SYMBOL_CATS,
        PanelKind::Emoji => EMOJI_CATS,
    }
}

/// 当前面板显示的项列表(符号/emoji 都按当前分类)。
fn current_items(state: &PanelState) -> &'static [&'static str] {
    let cats = cats_for(state.kind);
    cats[state.cat_idx.min(cats.len() - 1)].1
}

// ── 全局 commit 通道 ────────────────────────────────────────────────────────
struct CommitChannel {
    context: Arc<Mutex<Option<windows::Win32::UI::TextServices::ITfContext>>>,
    client_tid: Arc<Mutex<u32>>,
    composition: Arc<Mutex<Option<windows::Win32::UI::TextServices::ITfComposition>>>,
    composition_range: Arc<Mutex<Option<windows::Win32::UI::TextServices::ITfRange>>>,
}
static COMMIT: OnceLock<CommitChannel> = OnceLock::new();
// SAFETY: 单 STA 线程,同 StatusBarState 套路。
unsafe impl Send for CommitChannel {}
unsafe impl Sync for CommitChannel {}

pub(crate) fn bind_commit(
    context: Arc<Mutex<Option<windows::Win32::UI::TextServices::ITfContext>>>,
    client_tid: Arc<Mutex<u32>>,
    composition: Arc<Mutex<Option<windows::Win32::UI::TextServices::ITfComposition>>>,
    composition_range: Arc<Mutex<Option<windows::Win32::UI::TextServices::ITfRange>>>,
) {
    let _ = COMMIT.set(CommitChannel { context, client_tid, composition, composition_range });
}

fn commit_str(text: &str) {
    // 上屏走 SendInput(KEYEVENTF_UNICODE) 把字符直接发到前台窗口光标处。
    // 2026-09-01 根因:emoji/符号面板跑在状态条 owner 进程(常是 explorer),
    // 其 COMMIT 缓存的是 explorer 自己的 TSF context → InsertTextAtSelection
    // 写进 explorer 文档,写不进前台 app(记事本/网页/对话窗)。TSF 是每进程
    // 一份 DLL 全局状态,跨进程拿不到对方 context。SendInput 不依赖任何进程
    // 上下文,文本直达前台输入焦点,最通用。emoji 是 UTF-16 代理对,逐 u16 发。
    let mut inputs: Vec<INPUT> = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            let inp = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: unit,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            inputs.push(inp);
        }
    }
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    #[cfg(feature = "dllentry_log")]
    crate::com_class_factory::log_dll_entry(&format!(
        "Panel: commit '{}' SendInput {}/{}", text, sent, inputs.len()
    ));
}

// ── 窗口类注册 / 创建 ───────────────────────────────────────────────────────
const PANEL_CLASS: &str = "PrisirPanelWindow";

fn register_class() -> Result<()> {
    let hinst = unsafe { GetModuleHandleW(None) }?;
    let class_name: Vec<u16> = PANEL_CLASS.encode_utf16().chain(std::iter::once(0)).collect();
    let mut existing = WNDCLASSEXW::default();
    if unsafe { GetClassInfoExW(hinst, PCWSTR(class_name.as_ptr()), &mut existing) }.is_ok() {
        return Ok(());
    }
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(panel_wnd_proc),
        hInstance: hinst.into(),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }?,
        hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        return Err(Error::from_win32());
    }
    Ok(())
}

fn ensure_window(state: &Arc<Mutex<RefCell<PanelState>>>) -> Result<HWND> {
    let existing = state.lock().unwrap().borrow().hwnd;
    if let Some(hwnd) = existing {
        if unsafe { IsWindow(hwnd) }.as_bool() {
            return Ok(hwnd);
        }
        state.lock().unwrap().borrow_mut().hwnd = None;
    }
    register_class()?;
    let hinst = unsafe { GetModuleHandleW(None) }?;
    let class_name: Vec<u16> = PANEL_CLASS.encode_utf16().chain(std::iter::once(0)).collect();
    let title: Vec<u16> = "PrisirPanel".encode_utf16().chain(std::iter::once(0)).collect();
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            0, 0, 10, 10,
            None, None, hinst, None,
        )
    }?;
    state.lock().unwrap().borrow_mut().hwnd = Some(hwnd);
    Ok(hwnd)
}

/// 显示指定种类面板(kind 相同且已显示 = 关掉,toggle)。
pub(crate) fn toggle_panel(kind: PanelKind, anchor_hwnd: Option<HWND>) {
    let state = global_panel_state();
    let (cur_kind, cur_visible) = {
        let g = state.lock().unwrap();
        let s = g.borrow();
        (s.kind, s.visible)
    };
    if cur_visible && cur_kind == kind {
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!("Panel: toggle OFF kind={:?} (was visible)", kind));
        hide(&state);
        return;
    }
    #[cfg(feature = "dllentry_log")]
    crate::com_class_factory::log_dll_entry(&format!("Panel: toggle ON kind={:?} (cur_visible={} cur_kind={:?})", kind, cur_visible, cur_kind));
    // 默认锚在状态条左下角。
    let (ax, ay) = if let Some(bar) = anchor_hwnd {
        let mut rc = RECT::default();
        unsafe { let _ = GetWindowRect(bar, &mut rc); }
        (rc.left, rc.bottom + 4)
    } else {
        let g = state.lock().unwrap();
        let s = g.borrow();
        (s.x, s.y)
    };
    let n = {
        let g = state.lock().unwrap();
        let s = g.borrow();
        current_items(&s).len()
    };
    let w = panel_width();
    let h = panel_height(n, kind);
    // 边界校正:若超出所在显示器工作区则收进来 —
    // 底部放不下就翻到状态条上方,右侧超出就左移贴右边,左/上越界就贴左/上边。
    let (x, y) = clamp_to_work_area(anchor_hwnd, ax, ay, w, h);
    {
        let g = state.lock().unwrap();
        let mut s = g.borrow_mut();
        s.kind = kind;
        s.x = x;
        s.y = y;
        if kind != cur_kind {
            s.cat_idx = 0; // 切面板重置分类
        }
        s.scroll_row = 0; // 切面板/重开重置滚动
    }
    if let Ok(hwnd) = ensure_window(&state) {
        unsafe {
            let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            let _ = InvalidateRect(hwnd, None, true);
        }
        state.lock().unwrap().borrow_mut().visible = true;
        #[cfg(feature = "dllentry_log")]
        crate::com_class_factory::log_dll_entry(&format!("Panel: show n={} at {},{}", n, x, y));
    }
}

/// 把面板 (x,y,w,h) 收进锚点窗口所在显示器的工作区(避开任务栏)。
/// anchor_hwnd 是状态条:底部放不下时翻到状态条上方(anchor.top - h - 4),
/// 否则保持状态条下方。水平方向右超左移、左越贴边。无锚点用当前面板位置。
fn clamp_to_work_area(anchor_hwnd: Option<HWND>, x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    // 工作区:优先锚点窗口所在显示器,否则主屏。
    let mut work = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
    let mut anchor_top: Option<i32> = None;
    unsafe {
        let mon = if let Some(bar) = anchor_hwnd {
            let mut brc = RECT::default();
            let _ = GetWindowRect(bar, &mut brc);
            anchor_top = Some(brc.top);
            MonitorFromWindow(bar, MONITOR_DEFAULTTONEAREST)
        } else {
            MonitorFromWindow(None, MONITOR_DEFAULTTONEAREST)
        };
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            work = mi.rcWork;
        }
    }
    let mut nx = x;
    let mut ny = y;
    // 垂直:底部放不下 → 翻到锚点上方;翻上也放不下才贴工作区顶。
    if ny + h > work.bottom {
        if let Some(atop) = anchor_top {
            let above = atop - h - 4;
            ny = if above >= work.top { above } else { (work.bottom - h).max(work.top) };
        } else {
            ny = (work.bottom - h).max(work.top);
        }
    }
    // 水平:右超 → 贴右边;左越 → 贴左边。
    if nx + w > work.right {
        nx = (work.right - w).max(work.left);
    }
    if nx < work.left {
        nx = work.left;
    }
    (nx, ny)
}

pub(crate) fn hide(state: &Arc<Mutex<RefCell<PanelState>>>) {
    let hwnd = state.lock().unwrap().borrow_mut().hwnd.take();
    if let Some(hwnd) = hwnd {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = SendMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    state.lock().unwrap().borrow_mut().visible = false;
}

pub(crate) fn destroy(state: &Arc<Mutex<RefCell<PanelState>>>) {
    hide(state);
}

// ── 命中测试(左侧分类列表 + 右侧网格)─────────────────────────────────────
/// 分类列表命中(仅符号面板有分类列表,在左侧)。返分类索引。
fn hit_cat(cx: i32, cy: i32, kind: PanelKind) -> Option<usize> {
    if cx < PAD || cx >= PAD + CAT_W || cy < PAD {
        return None;
    }
    let idx = ((cy - PAD) / CAT_ROW_H) as usize;
    if idx < cats_for(kind).len() { Some(idx) } else { None }
}

/// 网格项命中(网格在分类列表右侧)。返当前分类内的项索引。
/// scroll_row = 已滚过的行数(滚轮),可见第 row 行 = 实际第 row+scroll_row 行。
fn hit_item(cx: i32, cy: i32, n_items: usize, _kind: PanelKind, scroll_row: i32) -> Option<usize> {
    let grid_x0 = PAD + CAT_W;
    if cx < grid_x0 || cy < PAD {
        return None;
    }
    let col = (cx - grid_x0) / CELL;
    let row = (cy - PAD) / CELL;
    if col < 0 || col >= COLS || row < 0 || row >= MAX_ROWS {
        return None;
    }
    let idx = ((row + scroll_row) * COLS + col) as usize;
    if idx < n_items { Some(idx) } else { None }
}

// ── 窗口过程 ────────────────────────────────────────────────────────────────
unsafe extern "system" fn panel_wnd_proc(
    hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => { paint_panel(hwnd); LRESULT(0) }
        WM_CLOSE => { let _ = DestroyWindow(hwnd); LRESULT(0) }
        WM_MOUSEWHEEL => {
            // 滚轮滚动网格(超出 MAX_ROWS 的符号,如特殊 169 个 ≈13 行)。
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let state = global_panel_state();
            {
                let g = state.lock().unwrap();
                let mut s = g.borrow_mut();
                let total_rows = ((current_items(&s).len() as i32) + COLS - 1) / COLS;
                let max_scroll = (total_rows - MAX_ROWS).max(0);
                if delta > 0 {
                    s.scroll_row = (s.scroll_row - 1).max(0);
                } else {
                    s.scroll_row = (s.scroll_row + 1).min(max_scroll);
                }
            }
            let _ = InvalidateRect(hwnd, None, true);
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTCLIENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => {
            let cx = (lparam.0 & 0xFFFF) as i16 as i32;
            let cy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let state = global_panel_state();
            let kind = { state.lock().unwrap().borrow().kind };
            // 符号/emoji 面板都有左侧分类列表,先判分类命中。
            if let Some(cat) = hit_cat(cx, cy, kind) {
                state.lock().unwrap().borrow_mut().cat_idx = cat;
                state.lock().unwrap().borrow_mut().scroll_row = 0; // 切分类回顶
                // 分类项数变了,重排窗口高度。
                let n = {
                    let g = state.lock().unwrap();
                    let s = g.borrow();
                    current_items(&s).len()
                };
                let (x, y) = {
                    let g = state.lock().unwrap();
                    let s = g.borrow();
                    (s.x, s.y)
                };
                let w = panel_width();
                let h = panel_height(n, kind);
                let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                let _ = InvalidateRect(hwnd, None, true);
                return LRESULT(0);
            }
            // 网格项命中 → 上屏。
            let (items, scroll_row) = {
                let g = state.lock().unwrap();
                let s = g.borrow();
                (current_items(&s), s.scroll_row)
            };
            if let Some(idx) = hit_item(cx, cy, items.len(), kind, scroll_row) {
                let text = items[idx];
                commit_str(text);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn paint_panel(hwnd: HWND) {
    let state = global_panel_state();
    let (kind, cat_idx, scroll_row) = {
        let g = state.lock().unwrap();
        let s = g.borrow();
        (s.kind, s.cat_idx, s.scroll_row)
    };
    let items = {
        let g = state.lock().unwrap();
        let s = g.borrow();
        current_items(&s)
    };

    let mut ps = PAINTSTRUCT::default();
    unsafe {
        // emoji 且 D2D 就绪 → 整面板走 D2D(背景/分类/网格/彩色字形一次画完),
        // 不做任何 GDI 绘制,消除同 hwnd 混画导致的覆盖/分类文字被刷黑。
        let use_color_emoji = kind == PanelKind::Emoji && ensure_d2d(hwnd);
        if use_color_emoji {
            // 仍需 BeginPaint/EndPaint 配对清掉 WM_PAINT 更新区。
            let hdc = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            let _ = hdc;
            let _ = d2d_draw_emoji_grid(hwnd, items, scroll_row, cat_idx);
            return;
        }

        // ── 符号面板:纯 GDI 路径 ──
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let bg = CreateSolidBrush(COLORREF(0x00FFFFFF));
        FillRect(hdc, &rc, bg);
        let _ = DeleteObject(bg);

        SetBkMode(hdc, TRANSPARENT);
        let grid_x0 = PAD + CAT_W;

        // 左侧分类列表(符号/emoji 都有分类)。分类名统一用雅黑(UI 字体)。
        let cats = cats_for(kind);
        let cat_font = make_ui_font(hdc, 16, "Microsoft YaHei");
        for (i, (name, _)) in cats.iter().enumerate() {
            let selected = i == cat_idx;
            let top = PAD + (i as i32) * CAT_ROW_H;
            let mut crc = RECT { left: PAD, top, right: PAD + CAT_W, bottom: top + CAT_ROW_H };
            if selected {
                let sel = CreateSolidBrush(COLORREF(0x00F0E4D4)); // 浅蓝底(0xBGR)
                FillRect(hdc, &mut crc, sel);
                let _ = DeleteObject(sel);
            }
            SetTextColor(hdc, COLORREF(if selected { 0x002A7FC4 } else { 0x00666666 }));
            let wide: Vec<u16> = name.encode_utf16().collect();
            let mut w2 = wide.clone();
            DrawTextW(hdc, &mut w2, &mut crc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        }
        if let Some((f, old)) = cat_font {
            SelectObject(hdc, old);
            let _ = DeleteObject(f);
        }

        // 符号面板用 Microsoft YaHei(CJK 字体,对几何/箭头/CJK标点/日文/韩文/
        // 俄文/制表/带圈序号覆盖远好于 Segoe UI Symbol,且 GDI 对缺字形有字体链
        // 回退兜底;解决用户实测「面板方框但记事本能显示」的 Segoe UI Symbol 覆盖缺口)。
        SetTextColor(hdc, COLORREF(0x00333333));
        let glyph_font = make_ui_font(hdc, 18, "Microsoft YaHei");
        let skip = (scroll_row * COLS) as usize;
        let total_rows = ((items.len() as i32) + COLS - 1) / COLS;
        let vis_rows = (total_rows - scroll_row).min(MAX_ROWS);
        // 搜狗式网格线(每格一个框):浅灰横竖线,先画线再写字。
        let grid_pen = CreatePen(PS_SOLID, 1, COLORREF(0x00DDDDDD));
        let old_pen = SelectObject(hdc, grid_pen);
        let gw = COLS * CELL;
        let gh = vis_rows * CELL;
        for r in 0..=vis_rows {
            let y = PAD + r * CELL;
            MoveToEx(hdc, grid_x0, y, None);
            LineTo(hdc, grid_x0 + gw, y);
        }
        for c in 0..=COLS {
            let x = grid_x0 + c * CELL;
            MoveToEx(hdc, x, PAD, None);
            LineTo(hdc, x, PAD + gh);
        }
        SelectObject(hdc, old_pen);
        let _ = DeleteObject(grid_pen);
        for (i, it) in items.iter().enumerate().skip(skip) {
            let vis = i - skip;
            let col = (vis as i32) % COLS;
            let row = (vis as i32) / COLS;
            if row >= MAX_ROWS { break; }
            let cx = grid_x0 + col * CELL;
            let cy = PAD + row * CELL;
            let mut crc = RECT { left: cx, top: cy, right: cx + CELL, bottom: cy + CELL };
            let wide: Vec<u16> = it.encode_utf16().collect();
            let mut w2 = wide.clone();
            DrawTextW(hdc, &mut w2, &mut crc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        }
        // 还原并删除字形字体。
        if let Some((f, old)) = glyph_font {
            SelectObject(hdc, old);
            let _ = DeleteObject(f);
        }
        let _ = EndPaint(hwnd, &ps);
    }
}
