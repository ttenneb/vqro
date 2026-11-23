//! One installed testing plugin, admitted separately from untrusted native argv.
use std::path::{Path, PathBuf};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub const COMMAND: &str = "vqro-testing-status";
#[derive(Clone)]
pub struct Plugin { pub path: PathBuf, skill: PathBuf, principal: Option<String>, loaded: bool }
impl Plugin {
    pub fn validate(path: &Path, root: &Path, principal: Option<String>) -> Result<Self> {
        let root = root.canonicalize().context("installed extensions directory unavailable")?;
        let expected = root.join("tier1/vqro-testing.ts");
        let path = path.canonicalize().context("testing plugin missing")?;
        ensure!(path == expected, "testing_plugin must be installed tier1/vqro-testing.ts");
        let skill = root.join("tier1/skills/vqro-setup-testing/SKILL.md").canonicalize().context("testing skill missing")?;
        ensure!(path.to_str().is_some_and(|p| p.len() <= 4096) && skill.to_str().is_some_and(|p| p.len() <= 4096), "testing paths exceed native inventory bounds");
        ensure!(path.starts_with(&root) && skill.starts_with(&root) && path.is_file() && skill.is_file(),
                "testing plugin and skill must be regular files inside installed extensions");
        Ok(Self { path, skill, principal: principal.filter(|p| !p.is_empty() && p.len() <= 128), loaded: false })
    }
    pub fn append(&self, argv: &mut Vec<String>) {
        argv.extend(["-e".into(), self.path.to_string_lossy().into_owned(), "--skill".into(), self.skill.to_string_lossy().into_owned()]);
    }
    pub fn observe(&mut self, rows: Option<&Vec<Value>>) {
        // Both reports come from this native process. Flags or a matching command name alone prove nothing.
        self.loaded = rows.is_some_and(|rows| {
            rows.iter().any(|r| r["name"] == COMMAND && r["source"] == "extension" && r["sourceInfo"]["path"].as_str() == self.path.to_str()) &&
            rows.iter().any(|r| r["name"] == "skill:vqro-setup-testing" && r["source"] == "skill" && r["sourceInfo"]["path"].as_str() == self.skill.to_str())
        });
    }
    pub fn fact(&self) -> Value {
        json!({"state": if self.loaded {"loaded"} else {"unverified"}, "path": self.path,
               "skill": self.skill, "principal": self.principal, "source": "rpc:get_commands"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installed_only_and_native_evidence_not_argv() {
        let dir = tempfile::tempdir().unwrap(); let root = dir.path().canonicalize().unwrap();
        let skill = root.join("tier1/skills/vqro-setup-testing/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap(); std::fs::write(&skill, "skill").unwrap();
        let path = root.join("tier1/vqro-testing.ts"); std::fs::write(&path, "plugin").unwrap();
        assert!(Plugin::validate(&root.join("outside.ts"), &root, None).is_err());
        assert!(Plugin::validate(dir.path(), &root, None).is_err());
        let mut plugin = Plugin::validate(&path, &root, Some("p:test".into())).unwrap();
        let mut argv = vec!["pi".into()]; plugin.append(&mut argv);
        assert!(argv.contains(&"-e".into())); assert_eq!(plugin.fact()["state"], "unverified");
        let mut rows = vec![json!({"name":COMMAND,"source":"extension","sourceInfo":{"path":path}})];
        plugin.observe(Some(&rows)); assert_eq!(plugin.fact()["state"], "unverified");
        rows.push(json!({"name":"skill:vqro-setup-testing","source":"skill","sourceInfo":{"path":skill}}));
        plugin.observe(Some(&rows)); assert_eq!(plugin.fact()["state"], "loaded");
        rows[0]["sourceInfo"]["path"] = json!(root.join("other.ts"));
        plugin.observe(Some(&rows)); assert_eq!(plugin.fact()["state"], "unverified");
        plugin.observe(None); assert_eq!(plugin.fact()["state"], "unverified");
    }
    #[test]
    fn native_environment_adds_only_the_two_dedicated_selectors() {
        let env = ["VQRO_SOCKET", "VQRO_PRINCIPAL", "VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR",
                   "VQRO_TESTING_EXTENSIONS_DIR", "VQRO_EXTENSION_TOKEN", "VQRO_BRIDGE_TOKEN", "HERDR_SOCKET"]
            .into_iter().map(|key| (key.to_owned(), "fixture".to_owned())).collect();
        let mut default = tokio::process::Command::new("fixture");
        vqro::native_process::native_env(&mut default, &env);
        let ordinary: Vec<_> = default.as_std().get_envs().map(|(key, _)| key.to_str().unwrap().to_owned()).collect();
        assert_eq!(ordinary.len(), 2, "shared/Codex environment must not inherit Pi-only selectors");
        assert!(!ordinary.contains(&"VQRO_TESTING_SOCKET".to_owned()));
        assert!(!ordinary.contains(&"VQRO_TESTING_STATE_DIR".to_owned()));
        let mut command = tokio::process::Command::new("fixture");
        super::super::native::native_env(&mut command, &env);
        let kept: Vec<_> = command.as_std().get_envs().map(|(key, _)| key.to_str().unwrap().to_owned()).collect();
        assert_eq!(kept.len(), 4);
        for key in ["VQRO_SOCKET", "VQRO_PRINCIPAL", "VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR"] {
            assert!(kept.contains(&key.to_owned()));
        }
    }
    #[cfg(unix)]
    #[test]
    fn installed_symlink_cannot_escape() {
        let dir = tempfile::tempdir().unwrap(); let outside = tempfile::tempdir().unwrap();
        let path = dir.path().join("tier1/vqro-testing.ts"); std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let target = outside.path().join("plugin.ts"); std::fs::write(&target,"plugin").unwrap();
        std::os::unix::fs::symlink(&target,&path).unwrap();
        assert!(Plugin::validate(&path,dir.path(),None).is_err());
    }
}
