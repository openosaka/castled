pub mod message {
	include!(concat!(env!("OUT_DIR"), "/message.rs"));
}

pub struct Tunnel {
}

// #[cfg(test)]
// mod tests {
//     use super::*;

//     #[test]
//     fn it_works() {
//         let result = add(2, 2);
//         assert_eq!(result, 4);
//     }
// }
