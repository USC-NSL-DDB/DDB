pub const APP_NAME: &str = "ddb";

#[inline]
pub fn get_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[inline]
pub fn get_proj_dirs() -> directories::ProjectDirs {
    use directories::ProjectDirs;
    ProjectDirs::from("com", "nsl", APP_NAME).expect("Failed to get project directories")
}

#[inline]
pub fn get_config_dir() -> std::path::PathBuf {
    get_proj_dirs().config_dir().to_path_buf()
}

#[inline]
pub fn get_user_id() -> String {
    let config_dir = get_config_dir();
    let user_id_file = config_dir.join("user_id");
    if let Ok(user_id) = std::fs::read_to_string(&user_id_file) {
        let user_id = user_id.trim();
        if !user_id.is_empty() {
            return user_id.to_string();
        }
    }

    use uuid::Uuid;
    let new_user_id = Uuid::new_v4().to_string();
    std::fs::create_dir_all(&config_dir).ok();
    std::fs::write(&user_id_file, &new_user_id).ok();
    new_user_id
}
