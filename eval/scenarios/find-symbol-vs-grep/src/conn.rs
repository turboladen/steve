pub struct Connection {
    pub host: String,
    pub port: u16,
}

impl Connection {
    pub fn new(host: String, port: u16) -> Self {
        Self { host, port }
    }
}
