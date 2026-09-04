// 入力ソースの状態。OSごとの判定結果を共通の形で表す。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputMode {
    Japanese,
    English,
    Other(String),
}
