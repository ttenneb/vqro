#[derive(Clone, Copy)]
pub(crate) enum ConfigEdit<'a> {
    Theme(&'a str),
    StatusIndicators(super::StatusIndicatorStyle),
    Sound(bool),
    ToastDelivery(super::ToastDelivery),
    ConfirmedTasksPython(&'a str),
}

impl ConfigEdit<'_> {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::ConfirmedTasksPython(_) => "Tasks Python confirmation",
            Self::Theme(_) => "theme",
            Self::StatusIndicators(_) => "status indicators",
            Self::Sound(_) => "sound setting",
            Self::ToastDelivery(_) => "toast setting",
        }
    }

    pub(crate) fn apply(self, content: &str) -> String {
        match self {
            Self::ConfirmedTasksPython(path) => {
                let Ok(mut document) = content.parse::<toml_edit::DocumentMut>() else {
                    return content.into();
                };
                if document
                    .get("extensions")
                    .is_some_and(|item| !item.is_table() && !item.is_inline_table())
                {
                    return content.into();
                }
                let decor = document
                    .get("extensions")
                    .and_then(|table| table.get("confirmed_tasks_python"))
                    .and_then(toml_edit::Item::as_value)
                    .map(|value| value.decor().clone());
                let mut value = toml_edit::Value::from(path);
                if let Some(decor) = decor {
                    *value.decor_mut() = decor;
                }
                document["extensions"]["confirmed_tasks_python"] = toml_edit::Item::Value(value);
                document.to_string()
            }
            Self::Theme(name) => {
                let content =
                    super::upsert_section_value(content, "theme", "name", &format!("\"{name}\""));
                super::upsert_section_bool(&content, "theme", "auto_switch", false)
            }
            Self::StatusIndicators(style) => super::upsert_section_value(
                content,
                "ui",
                "status_indicators",
                &format!("\"{}\"", style.as_str()),
            ),
            Self::Sound(enabled) => {
                super::upsert_section_bool(content, "ui.sound", "enabled", enabled)
            }
            Self::ToastDelivery(delivery) => {
                let value = match delivery {
                    super::ToastDelivery::Off => "\"off\"",
                    super::ToastDelivery::Herdr => "\"herdr\"",
                    super::ToastDelivery::Terminal => "\"terminal\"",
                    super::ToastDelivery::System => "\"system\"",
                };
                let content = super::upsert_section_value(content, "ui.toast", "delivery", value);
                super::remove_section_key(&content, "ui.toast", "enabled")
            }
        }
    }
}

pub(crate) fn update_file_at(
    path: &std::path::Path,
    description: &str,
    update: impl FnOnce(&str) -> String,
) -> Result<(), String> {
    let content = config_before_edit(path, description)?;
    std::fs::write(path, update(&content))
        .map_err(|error| format!("failed to save {description}: {error}"))
}

fn config_before_edit(path: &std::path::Path, description: &str) -> Result<String, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create config directory: {error}"))?;
    }
    match super::io::read_optional_config(path) {
        Ok(Some(content)) => Ok(content),
        Ok(None) => Ok(String::new()),
        Err(error) => Err(format!(
            "failed to read config before saving {description}: {error}"
        )),
    }
}

pub(crate) fn try_update_file_at(
    path: &std::path::Path,
    description: &str,
    update: impl FnOnce(&str) -> Result<String, String>,
) -> Result<(), String> {
    let content = config_before_edit(path, description)?;
    let updated = update(&content)?;
    // Keep the previous config intact on failure, including symlink targets.
    use std::io::Write;
    static NEXT_EDIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let target = crate::platform::resolve_write_target(path).map_err(|error| error.to_string())?;
    let sequence = NEXT_EDIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = target.with_extension(format!("toml.{}.{}.tmp", std::process::id(), sequence));
    // Do not remove a pre-existing temp file if exclusive creation fails.
    let mut file = crate::platform::create_private_state_file(&temporary)
        .map_err(|error| format!("failed to save {description}: {error}"))?;
    let result = (|| -> std::io::Result<()> {
        if let Ok(metadata) = std::fs::metadata(&target) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(updated.as_bytes())?;
        file.sync_all()?;
        drop(file);
        crate::platform::durable_replace_file(&temporary, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("failed to save {description}: {error}"))
}

pub(crate) fn write_edit(edit: ConfigEdit<'_>) -> Result<(), String> {
    update_file_at(&super::config_path(), edit.description(), |content| {
        edit.apply(content)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_file_at_does_not_move_a_leading_bom_into_the_file() {
        let dir = std::env::temp_dir().join(format!("herdr-config-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            b"\xEF\xBB\xBF[terminal]\ndefault_shell = \"pwsh.exe\"\n",
        )
        .unwrap();

        update_file_at(&path, "onboarding setting", |content| {
            crate::config::upsert_top_level_bool(content, "onboarding", false)
        })
        .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_dir_all(dir);

        assert!(
            !written.contains('\u{feff}'),
            "unexpected BOM in {written:?}"
        );
        assert!(
            toml::from_str::<toml::Value>(&written).is_ok(),
            "written config is not valid TOML: {written:?}"
        );
    }
}
