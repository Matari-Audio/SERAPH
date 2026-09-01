use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::Result;
use xai_grok_tools::implementations::skills::{
    discovery::{find_skill_md_paths, parse_skill_files},
    skill::{SkillRef, build_skill_block, build_skill_information, load_skill_content},
    types::{SkillInfo, SkillScope},
};

pub fn discover(cwd: &Path) -> HashMap<String, SkillInfo> {
    let mut files = Vec::new();
    for root in [".seraph/skills", ".agents/skills", ".codex/skills"] {
        files.extend(
            find_skill_md_paths(&cwd.join(root))
                .into_iter()
                .map(|path| (path, SkillScope::Local)),
        );
    }
    if let Some(home) = std::env::var_os("HOME") {
        for root in [".seraph/skills", ".agents/skills", ".codex/skills"] {
            files.extend(
                find_skill_md_paths(&Path::new(&home).join(root))
                    .into_iter()
                    .map(|path| (path, SkillScope::User)),
            );
        }
    }
    let mut skills = HashMap::new();
    for skill in parse_skill_files(files)
        .into_iter()
        .filter(|skill| skill.user_invocable)
    {
        skills.entry(skill.name.clone()).or_insert(skill);
    }
    skills
}

pub async fn expand(skills: &HashMap<String, SkillInfo>, text: String) -> Result<String> {
    let mut seen = HashSet::new();
    let referenced: Vec<_> = text
        .split_whitespace()
        .filter_map(|token| token.strip_prefix('$'))
        .filter_map(|name| skills.get(name))
        .filter(|skill| seen.insert(skill.path.clone()))
        .collect();
    let mut blocks = Vec::with_capacity(referenced.len());
    for skill in &referenced {
        let body = load_skill_content(skill)
            .await
            .map_err(anyhow::Error::msg)?;
        blocks.push(build_skill_block(&skill.name, "", &body));
    }
    if blocks.is_empty() {
        return Ok(text);
    }
    let refs: Vec<_> = referenced
        .iter()
        .map(|skill| SkillRef {
            name: &skill.name,
            path: &skill.path,
        })
        .collect();
    Ok(format!(
        "<user_query>\n{text}\n</user_query>\n{}",
        build_skill_information(&blocks, &refs)
    ))
}

pub fn commands(
    skills: &HashMap<String, SkillInfo>,
) -> Vec<agent_client_protocol::AvailableCommand> {
    let mut skills: Vec<_> = skills.values().collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
        .into_iter()
        .map(|skill| {
            agent_client_protocol::AvailableCommand::new(
                skill.name.clone(),
                skill.description.clone(),
            )
            .meta(
                serde_json::json!({ "path": skill.path, "scope": skill.scope })
                    .as_object()
                    .cloned()
                    .expect("skill metadata is an object"),
            )
        })
        .collect()
}
