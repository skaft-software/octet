//! Graphical theme options and colour roles.

use super::*;

pub(super) fn graphical_themes(config: &Config) -> anyhow::Result<(Vec<ThemeOption>, ThemeId)> {
    const MAX_GRAPHICAL_THEMES: usize = 64;

    let selected_name = crate::tui::theme::DEFAULT_THEME_NAME.to_owned();
    let mut names = crate::tui::theme::available_themes(config);
    names.retain(|name| name != &selected_name);
    names.insert(0, selected_name.clone());

    let mut themes = Vec::new();
    for name in names.into_iter().take(MAX_GRAPHICAL_THEMES) {
        let Ok(theme) = crate::tui::theme::load_named_theme(&name, config) else {
            continue;
        };
        themes.push(graphical_theme_option(&name, &theme, config)?);
    }
    if themes.is_empty() {
        let theme = crate::tui::theme::load_theme(config);
        themes.push(graphical_theme_option(&selected_name, &theme, config)?);
    }
    let selected_theme_id = graphical_theme_id(&selected_name)?;
    if !themes.iter().any(|theme| theme.id == selected_theme_id) {
        anyhow::bail!("selected graphical theme was not projected");
    }
    Ok((themes, selected_theme_id))
}

pub(super) fn graphical_theme_id(name: &str) -> anyhow::Result<ThemeId> {
    ThemeId::new(format!("theme-{}", &stable_hash(name.as_bytes())[..24]))
        .map_err(anyhow::Error::msg)
}

pub(super) fn graphical_theme_option(
    name: &str,
    theme: &crate::tui::theme::OctetTheme,
    config: &Config,
) -> anyhow::Result<ThemeOption> {
    const BUILT_IN_ROLES: &[&str] = &[
        "text",
        "muted",
        "subtle",
        "accent",
        "success",
        "warning",
        "error",
        "heading",
        "emphasis",
        "strong",
        "inline_code",
        "code",
        "quote",
        "border",
        "link",
        "list_marker",
        "diff_add",
        "diff_remove",
        "diff_context",
        "diff_hunk",
        "diff_header",
        "syntax_comment",
        "syntax_keyword",
        "syntax_function",
        "syntax_variable",
        "syntax_string",
        "syntax_number",
        "syntax_type",
        "syntax_operator",
        "syntax_punctuation",
    ];

    let mut role_names = BUILT_IN_ROLES
        .iter()
        .map(|role| (*role).to_owned())
        .collect::<Vec<_>>();
    role_names.extend(theme.semantic_role_names().map(str::to_owned));
    role_names.sort();
    role_names.dedup();
    // Each role may contribute one foreground and one background token.
    role_names.truncate(128);

    let mut colors = BTreeMap::new();
    let mut roles = BTreeMap::new();
    for (index, role_name) in role_names.into_iter().enumerate() {
        let Ok(role) = SemanticRole::new(role_name.clone()) else {
            continue;
        };
        let style = theme.semantic_style(&role_name);
        let foreground = graphical_color_token(&mut colors, index, "foreground", style.foreground);
        let background = graphical_color_token(&mut colors, index, "background", style.background);
        roles.insert(role, graphical_role_style(style, foreground, background));
    }

    let source = match theme.source() {
        crate::tui::theme::ThemeSource::CompiledDefault
        | crate::tui::theme::ThemeSource::CompiledCards
        | crate::tui::theme::ThemeSource::CompiledStill => ThemeSourceClass::Bundled,
        crate::tui::theme::ThemeSource::File(path) if path.starts_with(&config.workspace) => {
            ThemeSourceClass::Project
        }
        crate::tui::theme::ThemeSource::File(_) => ThemeSourceClass::Global,
    };
    let scheme = match theme.background() {
        crate::tui::theme::TerminalBackground::Dark => ColorScheme::Dark,
        crate::tui::theme::TerminalBackground::Light => ColorScheme::Light,
        crate::tui::theme::TerminalBackground::Unknown => ColorScheme::Unknown,
    };
    let density = match theme.layout().density {
        crate::tui::theme::ThemeDensity::Compact => ThemeDensity::Compact,
        crate::tui::theme::ThemeDensity::Comfortable => ThemeDensity::Comfortable,
        crate::tui::theme::ThemeDensity::Airy => ThemeDensity::Airy,
    };
    let display_name = if theme.metadata().name.trim().is_empty() {
        name
    } else {
        &theme.metadata().name
    };
    let option = ThemeOption {
        id: graphical_theme_id(name)?,
        theme: ThemeDto {
            name: bounded_text(display_name, 128),
            source,
            revision: 1,
            scheme,
            density,
            motion: ThemeMotion::Full,
            typography: ThemeTypography {
                body_family: "system-ui".into(),
                mono_family: "ui-monospace".into(),
                body_size: 17,
                display_ratio_milli: 1235,
            },
            colors,
            roles,
        },
    };
    option.validate().map_err(anyhow::Error::msg)?;
    Ok(option)
}

pub(super) fn graphical_color_token(
    colors: &mut BTreeMap<String, ThemeColor>,
    index: usize,
    channel: &str,
    color: TuiColor,
) -> Option<String> {
    let projected = match color {
        TuiColor::Default => return Some("default".into()),
        TuiColor::Ansi16(index) | TuiColor::Indexed(index) => ThemeColor::Ansi { index },
        TuiColor::Rgb(red, green, blue) => ThemeColor::Rgb { red, green, blue },
    };
    let token = format!("role.{index}.{channel}");
    colors.insert(token.clone(), projected);
    Some(token)
}

pub(super) fn graphical_role_style(
    style: TuiTextStyle,
    mut foreground: Option<String>,
    mut background: Option<String>,
) -> ThemeRoleStyle {
    if style.attributes.inverse {
        std::mem::swap(&mut foreground, &mut background);
    }
    ThemeRoleStyle {
        foreground,
        background,
        bold: style.attributes.bold,
        dim: style.attributes.dim,
        italic: style.attributes.italic,
        underline: style.attributes.underline,
        strikethrough: style.attributes.strikethrough,
    }
}
