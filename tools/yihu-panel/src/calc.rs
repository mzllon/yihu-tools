//! 算式求值：递归下降，支持 `+ - * / % ^`、括号与一元正负号。
//!
//! 仅在整段输入恰好是一个合法算式时返回 Some；任何残余字符、
//! 缺括号、除零/非有限结果都返回 None。无外部依赖。

/// 求值入口。输入会先去除全部空白字符。
pub fn evaluate(input: &str) -> Option<f64> {
    let tokens: Vec<char> = input.chars().filter(|c| !c.is_whitespace()).collect();
    if tokens.is_empty() {
        return None;
    }
    let mut p = Parser { t: &tokens, pos: 0 };
    let v = p.expr()?;
    if p.pos == tokens.len() && v.is_finite() {
        Some(v)
    } else {
        None
    }
}

/// 结果格式化：整数不带小数点，小数去掉末尾多余的 0。
pub fn format_result(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.10}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

struct Parser<'a> {
    t: &'a [char],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.t.get(self.pos).copied()
    }

    /// 加减级（最低优先级）
    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while matches!(self.peek(), Some('+') | Some('-')) {
            let op = self.t[self.pos];
            self.pos += 1;
            let r = self.term()?;
            v = if op == '+' { v + r } else { v - r };
        }
        Some(v)
    }

    /// 乘除模级
    fn term(&mut self) -> Option<f64> {
        let mut v = self.factor()?;
        while matches!(self.peek(), Some('*') | Some('/') | Some('%')) {
            let op = self.t[self.pos];
            self.pos += 1;
            let r = self.factor()?;
            v = match op {
                '*' => v * r,
                '/' => v / r,
                _ => v % r,
            };
        }
        Some(v)
    }

    /// 一元正负号（绑定比幂低：-2^2 = -4；指数侧允许 2^-3）
    fn factor(&mut self) -> Option<f64> {
        match self.peek() {
            Some('-') => {
                self.pos += 1;
                Some(-self.factor()?)
            }
            Some('+') => {
                self.pos += 1;
                self.factor()
            }
            _ => self.power(),
        }
    }

    /// 幂（右结合）
    fn power(&mut self) -> Option<f64> {
        let v = self.primary()?;
        if self.peek() == Some('^') {
            self.pos += 1;
            Some(v.powf(self.factor()?))
        } else {
            Some(v)
        }
    }

    fn primary(&mut self) -> Option<f64> {
        match self.peek() {
            Some('(') => {
                self.pos += 1;
                let v = self.expr()?;
                if self.peek() == Some(')') {
                    self.pos += 1;
                    Some(v)
                } else {
                    None
                }
            }
            Some(c) if c.is_ascii_digit() || c == '.' => {
                let start = self.pos;
                while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == '.') {
                    self.pos += 1;
                }
                self.t[start..self.pos].iter().collect::<String>().parse().ok()
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_arithmetic() {
        assert_eq!(evaluate("1+2*3").map(|v| v as i64), Some(7));
        assert_eq!(evaluate("(1+2)*3").map(|v| v as i64), Some(9));
        assert_eq!(evaluate("10-4-3").map(|v| v as i64), Some(3));
        assert_eq!(evaluate("10%3").map(|v| v as i64), Some(1));
        assert_eq!(evaluate("2^3^2").map(|v| v as i64), Some(512)); // 右结合
        assert_eq!(evaluate("-2^2").map(|v| v as i64), Some(-4));
        assert_eq!(evaluate("3-(-2)").map(|v| v as i64), Some(5));
        assert_eq!(evaluate(" 3.5 * 2 ").map(|v| v as i64), Some(7));
        assert_eq!(evaluate("1/4").map(|v| v * 4.0), Some(1.0));
        assert_eq!(evaluate(".5*4").map(|v| v as i64), Some(2));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(evaluate("1+"), None);
        assert_eq!(evaluate("abc"), None);
        assert_eq!(evaluate("(1+2"), None); // 缺右括号
        assert_eq!(evaluate("1/0"), None); // inf
        assert_eq!(evaluate(""), None);
        assert_eq!(evaluate("yihu"), None);
        assert_eq!(evaluate("1..2"), None);
    }

    #[test]
    fn format_results() {
        assert_eq!(format_result(7.0), "7");
        assert_eq!(format_result(-4.0), "-4");
        assert_eq!(format_result(1.5), "1.5");
        assert_eq!(format_result(1.0 / 3.0), "0.3333333333");
    }
}
