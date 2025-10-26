//! CLI Regression Test Harness
//!
//! This module provides infrastructure for capturing and comparing CLI outputs
//! to ensure the command flow refactor preserves existing behavior.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// Represents a captured CLI command and its output
    #[derive(Debug, Clone)]
    struct CliSnapshot {
        command: String,
        output: String,
        external_token: Option<u64>,
    }

    impl CliSnapshot {
        fn new(command: String, output: String, external_token: Option<u64>) -> Self {
            Self {
                command,
                output,
                external_token,
            }
        }

        /// Compare two snapshots, ignoring timestamps and session-specific IDs
        fn compare(&self, other: &CliSnapshot) -> Result<(), String> {
            if self.command != other.command {
                return Err(format!(
                    "Command mismatch: '{}' vs '{}'",
                    self.command, other.command
                ));
            }

            if self.external_token != other.external_token {
                return Err(format!(
                    "Token mismatch: {:?} vs {:?}",
                    self.external_token, other.external_token
                ));
            }

            // Normalize outputs for comparison (remove timestamps, normalize whitespace)
            let normalized_self = Self::normalize_output(&self.output);
            let normalized_other = Self::normalize_output(&other.output);

            if normalized_self != normalized_other {
                return Err(format!(
                    "Output mismatch:\nExpected:\n{}\nActual:\n{}",
                    normalized_self, normalized_other
                ));
            }

            Ok(())
        }

        /// Normalize output by removing timestamps and normalizing whitespace
        fn normalize_output(output: &str) -> String {
            // Remove common timestamp patterns
            let mut normalized = output.to_string();

            // Remove lines with timestamps (common patterns)
            normalized = normalized
                .lines()
                .filter(|line| !line.contains("time=") && !line.contains("timestamp"))
                .collect::<Vec<_>>()
                .join("\n");

            // Normalize whitespace
            normalized
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string()
        }
    }

    /// Snapshot collection for regression testing
    struct SnapshotCollection {
        snapshots: Vec<CliSnapshot>,
    }

    impl SnapshotCollection {
        fn new() -> Self {
            Self {
                snapshots: Vec::new(),
            }
        }

        fn add(&mut self, snapshot: CliSnapshot) {
            self.snapshots.push(snapshot);
        }

        fn compare(&self, other: &SnapshotCollection) -> Result<(), Vec<String>> {
            let mut errors = Vec::new();

            if self.snapshots.len() != other.snapshots.len() {
                errors.push(format!(
                    "Snapshot count mismatch: {} vs {}",
                    self.snapshots.len(),
                    other.snapshots.len()
                ));
                return Err(errors);
            }

            for (i, (baseline, current)) in
                self.snapshots.iter().zip(other.snapshots.iter()).enumerate()
            {
                if let Err(e) = baseline.compare(current) {
                    errors.push(format!("Snapshot {} failed: {}", i, e));
                }
            }

            if errors.is_empty() {
                Ok(())
            } else {
                Err(errors)
            }
        }

        /// Save snapshots to file for baseline comparison
        #[allow(dead_code)]
        fn save_to_file(&self, path: &PathBuf) -> std::io::Result<()> {
            use std::fs::File;
            use std::io::Write;

            let mut file = File::create(path)?;
            for snapshot in &self.snapshots {
                writeln!(
                    file,
                    "COMMAND: {}\nTOKEN: {:?}\nOUTPUT:\n{}\n---",
                    snapshot.command, snapshot.external_token, snapshot.output
                )?;
            }
            Ok(())
        }

        /// Load snapshots from file
        #[allow(dead_code)]
        fn load_from_file(path: &PathBuf) -> std::io::Result<Self> {
            use std::fs;

            let content = fs::read_to_string(path)?;
            let mut collection = Self::new();

            // Simple parsing (can be made more robust)
            let entries: Vec<&str> = content.split("---").collect();
            for entry in entries {
                if entry.trim().is_empty() {
                    continue;
                }

                let lines: Vec<&str> = entry.lines().collect();
                let mut command = String::new();
                let mut token = None;
                let mut output = String::new();
                let mut in_output = false;

                for line in lines {
                    if line.starts_with("COMMAND: ") {
                        command = line.strip_prefix("COMMAND: ").unwrap().to_string();
                    } else if line.starts_with("TOKEN: ") {
                        let token_str = line.strip_prefix("TOKEN: ").unwrap();
                        if token_str != "None" {
                            token = token_str
                                .trim_matches(|c| c == 'S' || c == 'o' || c == 'm' || c == 'e' || c == '(' || c == ')')
                                .parse()
                                .ok();
                        }
                    } else if line.starts_with("OUTPUT:") {
                        in_output = true;
                    } else if in_output {
                        output.push_str(line);
                        output.push('\n');
                    }
                }

                if !command.is_empty() {
                    collection.add(CliSnapshot::new(command, output.trim().to_string(), token));
                }
            }

            Ok(collection)
        }
    }

    #[test]
    fn test_snapshot_comparison_identical() {
        let snapshot1 = CliSnapshot::new(
            "-thread-info".to_string(),
            "^done,threads=[{id=\"1\"}]".to_string(),
            Some(123),
        );

        let snapshot2 = CliSnapshot::new(
            "-thread-info".to_string(),
            "^done,threads=[{id=\"1\"}]".to_string(),
            Some(123),
        );

        assert!(snapshot1.compare(&snapshot2).is_ok());
    }

    #[test]
    fn test_snapshot_comparison_different_output() {
        let snapshot1 = CliSnapshot::new(
            "-thread-info".to_string(),
            "^done,threads=[{id=\"1\"}]".to_string(),
            Some(123),
        );

        let snapshot2 = CliSnapshot::new(
            "-thread-info".to_string(),
            "^done,threads=[{id=\"2\"}]".to_string(),
            Some(123),
        );

        assert!(snapshot1.compare(&snapshot2).is_err());
    }

    #[test]
    fn test_snapshot_normalization() {
        let output1 = "^done,  threads=[{id=\"1\"}]  ";
        let output2 = "^done,threads=[{id=\"1\"}]";

        let normalized1 = CliSnapshot::normalize_output(output1);
        let normalized2 = CliSnapshot::normalize_output(output2);

        assert_eq!(normalized1, normalized2);
    }

    #[test]
    fn test_collection_comparison() {
        let mut collection1 = SnapshotCollection::new();
        collection1.add(CliSnapshot::new(
            "-thread-info".to_string(),
            "^done".to_string(),
            None,
        ));

        let mut collection2 = SnapshotCollection::new();
        collection2.add(CliSnapshot::new(
            "-thread-info".to_string(),
            "^done".to_string(),
            None,
        ));

        assert!(collection1.compare(&collection2).is_ok());
    }
}
