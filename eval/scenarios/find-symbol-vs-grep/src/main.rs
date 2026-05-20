mod conn;

use conn::Connection;

fn main() {
    let primary = Connection::new("localhost".into(), 5432);
    println!("{}:{}", primary.host, primary.port);
    let _replica = Connection {
        host: "replica.example.com".into(),
        port: 5432,
    };
}
