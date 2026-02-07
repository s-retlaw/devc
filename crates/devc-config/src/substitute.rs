//! Variable substitution for devcontainer.json values
//!
//! Supports the devcontainer spec variables:
//! - `${localWorkspaceFolder}` — host workspace path
//! - `${containerWorkspaceFolder}` — container workspace path
//! - `${localWorkspaceFolderBasename}` — last segment of host workspace path
//! - `${containerWorkspaceFolderBasename}` — last segment of container workspace path
//! - `${localEnv:VAR}` — host environment variable
//! - `${localEnv:VAR:default}` — host environment variable with fallback
//! - `${containerEnv:VAR}` — left as-is (resolved at runtime)

use std::collections::HashMap;
use std::path::Path;

/// Context for variable substitution
#[derive(Debug, Clone)]
pub struct SubstitutionContext {
    pub local_workspace_folder: String,
    pub container_workspace_folder: String,
}

impl SubstitutionContext {
    pub fn new(local_workspace_folder: impl Into<String>, container_workspace_folder: impl Into<String>) -> Self {
        Self {
            local_workspace_folder: local_workspace_folder.into(),
            container_workspace_folder: container_workspace_folder.into(),
        }
    }
}

/// Substitute variables in a string
pub fn substitute(input: &str, ctx: &SubstitutionContext) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut var_name = String::new();
            let mut depth = 1;
            while let Some(&nc) = chars.peek() {
                if nc == '}' {
                    depth -= 1;
                    if depth == 0 {
                        chars.next(); // consume '}'
                        break;
                    }
                } else if nc == '{' {
                    depth += 1;
                }
                var_name.push(nc);
                chars.next();
            }
            result.push_str(&resolve_variable(&var_name, ctx));
        } else {
            result.push(c);
        }
    }

    result
}

fn resolve_variable(var: &str, ctx: &SubstitutionContext) -> String {
    match var {
        "localWorkspaceFolder" => ctx.local_workspace_folder.clone(),
        "containerWorkspaceFolder" => ctx.container_workspace_folder.clone(),
        "localWorkspaceFolderBasename" => basename(&ctx.local_workspace_folder),
        "containerWorkspaceFolderBasename" => basename(&ctx.container_workspace_folder),
        _ if var.starts_with("localEnv:") => {
            let rest = &var["localEnv:".len()..];
            if let Some((name, default)) = rest.split_once(':') {
                std::env::var(name).unwrap_or_else(|_| default.to_string())
            } else {
                std::env::var(rest).unwrap_or_default()
            }
        }
        _ if var.starts_with("containerEnv:") => {
            // Leave containerEnv variables as-is for runtime resolution
            format!("${{{}}}", var)
        }
        _ => {
            // Unknown variable, leave as-is
            format!("${{{}}}", var)
        }
    }
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Substitute variables in an optional string
pub fn substitute_opt(input: &Option<String>, ctx: &SubstitutionContext) -> Option<String> {
    input.as_ref().map(|s| substitute(s, ctx))
}

/// Substitute variables in a vec of strings
pub fn substitute_vec(input: &[String], ctx: &SubstitutionContext) -> Vec<String> {
    input.iter().map(|s| substitute(s, ctx)).collect()
}

/// Substitute variables in a HashMap's values
pub fn substitute_map(input: &HashMap<String, String>, ctx: &SubstitutionContext) -> HashMap<String, String> {
    input.iter().map(|(k, v)| (k.clone(), substitute(v, ctx))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> SubstitutionContext {
        SubstitutionContext::new("/home/user/project", "/workspace")
    }

    #[test]
    fn test_basic_substitution() {
        let ctx = test_ctx();
        assert_eq!(substitute("${localWorkspaceFolder}", &ctx), "/home/user/project");
        assert_eq!(substitute("${containerWorkspaceFolder}", &ctx), "/workspace");
    }

    #[test]
    fn test_basename_substitution() {
        let ctx = test_ctx();
        assert_eq!(substitute("${localWorkspaceFolderBasename}", &ctx), "project");
        assert_eq!(substitute("${containerWorkspaceFolderBasename}", &ctx), "workspace");
    }

    #[test]
    fn test_local_env_substitution() {
        let ctx = test_ctx();
        std::env::set_var("DEVC_TEST_VAR", "hello");
        assert_eq!(substitute("${localEnv:DEVC_TEST_VAR}", &ctx), "hello");
        assert_eq!(substitute("${localEnv:DEVC_TEST_MISSING:fallback}", &ctx), "fallback");
        assert_eq!(substitute("${localEnv:DEVC_TEST_MISSING}", &ctx), "");
        std::env::remove_var("DEVC_TEST_VAR");
    }

    #[test]
    fn test_container_env_deferred() {
        let ctx = test_ctx();
        assert_eq!(substitute("${containerEnv:HOME}", &ctx), "${containerEnv:HOME}");
    }

    #[test]
    fn test_no_substitution() {
        let ctx = test_ctx();
        assert_eq!(substitute("plain string", &ctx), "plain string");
        assert_eq!(substitute("", &ctx), "");
    }

    #[test]
    fn test_multiple_substitutions() {
        let ctx = test_ctx();
        assert_eq!(
            substitute("${localWorkspaceFolder}:${containerWorkspaceFolder}", &ctx),
            "/home/user/project:/workspace"
        );
    }

    #[test]
    fn test_mixed_text_and_variables() {
        let ctx = test_ctx();
        assert_eq!(
            substitute("source=${localWorkspaceFolder}/.cache,target=${containerWorkspaceFolder}/.cache", &ctx),
            "source=/home/user/project/.cache,target=/workspace/.cache"
        );
    }

    #[test]
    fn test_substitute_opt() {
        let ctx = test_ctx();
        assert_eq!(substitute_opt(&Some("${localWorkspaceFolder}".to_string()), &ctx), Some("/home/user/project".to_string()));
        assert_eq!(substitute_opt(&None, &ctx), None);
    }

    #[test]
    fn test_substitute_vec() {
        let ctx = test_ctx();
        let input = vec!["${localWorkspaceFolder}".to_string(), "plain".to_string()];
        let result = substitute_vec(&input, &ctx);
        assert_eq!(result, vec!["/home/user/project", "plain"]);
    }

    #[test]
    fn test_substitute_map() {
        let ctx = test_ctx();
        let mut input = HashMap::new();
        input.insert("key".to_string(), "${containerWorkspaceFolder}/bin".to_string());
        let result = substitute_map(&input, &ctx);
        assert_eq!(result.get("key").unwrap(), "/workspace/bin");
    }
}
