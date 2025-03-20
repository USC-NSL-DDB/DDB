#[macro_export]
macro_rules! handlers_map {
    ($( $key:expr => $handler:expr ),* $(,)?) => {
        {
            let map: DashMap<String, Box<dyn Handler>> = DashMap::new();
            $(
                map.insert($key.to_string(), Box::new($handler));
            )*
            map
        }
    };
}
