//! firepath CLI

/// Sum two operands, returning `None` when the result would overflow
pub fn add(left: u64, right: u64) -> Option<u64> {
    left.checked_add(right)
}

fn main() {
    match add(2, 2) {
        Some(sum) => println!("2 + 2 = {sum}"),
        None => eprintln!("addition overflowed"),
    }
}

#[cfg(test)]
mod tests {
    use super::add;

    #[test]
    fn sums_two_operands() {
        assert_eq!(add(2, 2), Some(4));
    }

    #[test]
    fn reports_overflow_instead_of_wrapping() {
        assert_eq!(add(u64::MAX, 1), None);
    }
}
