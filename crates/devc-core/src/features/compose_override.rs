//! Compose override YAML generator for devcontainer feature properties
//!
//! When features declare container properties (capAdd, securityOpt, init, privileged),
//! we generate a compose override file that Docker Compose merges additively.

use super::resolve::MergedFeatureProperties;

/// Generate a compose override YAML string for the given service and feature properties.
///
/// Returns `None` if no properties need overriding (all defaults).
/// The generated YAML is suitable for passing as an additional `-f` flag to `docker compose up`.
pub fn generate_compose_override(
    service_name: &str,
    props: &MergedFeatureProperties,
) -> Option<String> {
    if !props.has_container_properties() {
        return None;
    }

    let mut yaml = String::from("services:\n");
    yaml.push_str(&format!("  {}:\n", service_name));

    if !props.cap_add.is_empty() {
        yaml.push_str("    cap_add:\n");
        for cap in &props.cap_add {
            yaml.push_str(&format!("      - {}\n", cap));
        }
    }

    if !props.security_opt.is_empty() {
        yaml.push_str("    security_opt:\n");
        for opt in &props.security_opt {
            yaml.push_str(&format!("      - {}\n", opt));
        }
    }

    if props.init {
        yaml.push_str("    init: true\n");
    }

    if props.privileged {
        yaml.push_str("    privileged: true\n");
    }

    Some(yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_props_returns_none() {
        let props = MergedFeatureProperties::default();
        assert!(generate_compose_override("web", &props).is_none());
    }

    #[test]
    fn test_cap_add_only() {
        let props = MergedFeatureProperties {
            cap_add: vec!["SYS_PTRACE".to_string()],
            ..Default::default()
        };
        let yaml = generate_compose_override("app", &props).unwrap();
        assert_eq!(
            yaml,
            "services:\n  app:\n    cap_add:\n      - SYS_PTRACE\n"
        );
    }

    #[test]
    fn test_security_opt_only() {
        let props = MergedFeatureProperties {
            security_opt: vec!["seccomp=unconfined".to_string()],
            ..Default::default()
        };
        let yaml = generate_compose_override("web", &props).unwrap();
        assert_eq!(
            yaml,
            "services:\n  web:\n    security_opt:\n      - seccomp=unconfined\n"
        );
    }

    #[test]
    fn test_init_only() {
        let props = MergedFeatureProperties {
            init: true,
            ..Default::default()
        };
        let yaml = generate_compose_override("svc", &props).unwrap();
        assert_eq!(yaml, "services:\n  svc:\n    init: true\n");
    }

    #[test]
    fn test_privileged_only() {
        let props = MergedFeatureProperties {
            privileged: true,
            ..Default::default()
        };
        let yaml = generate_compose_override("svc", &props).unwrap();
        assert_eq!(yaml, "services:\n  svc:\n    privileged: true\n");
    }

    #[test]
    fn test_all_properties() {
        let props = MergedFeatureProperties {
            cap_add: vec!["SYS_PTRACE".to_string(), "NET_ADMIN".to_string()],
            security_opt: vec![
                "seccomp=unconfined".to_string(),
                "apparmor=unconfined".to_string(),
            ],
            init: true,
            privileged: true,
            ..Default::default()
        };
        let yaml = generate_compose_override("my-service", &props).unwrap();
        let expected = "\
services:
  my-service:
    cap_add:
      - SYS_PTRACE
      - NET_ADMIN
    security_opt:
      - seccomp=unconfined
      - apparmor=unconfined
    init: true
    privileged: true
";
        assert_eq!(yaml, expected);
    }

    #[test]
    fn test_no_booleans_set() {
        let props = MergedFeatureProperties {
            cap_add: vec!["SYS_PTRACE".to_string()],
            security_opt: vec!["seccomp=unconfined".to_string()],
            init: false,
            privileged: false,
            ..Default::default()
        };
        let yaml = generate_compose_override("app", &props).unwrap();
        // Should NOT include init or privileged lines
        assert!(!yaml.contains("init"));
        assert!(!yaml.contains("privileged"));
        assert!(yaml.contains("cap_add"));
        assert!(yaml.contains("security_opt"));
    }

    #[test]
    fn test_mounts_and_lifecycle_not_included() {
        use devc_config::{Command, Mount};

        let props = MergedFeatureProperties {
            mounts: vec![Mount::String(
                "type=volume,source=v,target=/data".to_string(),
            )],
            on_create_commands: vec![Command::String("echo hi".to_string())],
            ..Default::default()
        };
        // No container properties → returns None
        assert!(generate_compose_override("app", &props).is_none());
    }
}
