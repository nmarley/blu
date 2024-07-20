/// Get the patterns from the .bluignore file
pub fn get_bluignore_patterns() -> Vec<String> {
    if let Ok(bluignore) = std::fs::read_to_string(".bluignore") {
        bluignore
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.to_string())
            .collect()
    } else {
        vec![]
    }
}
