//! PostgreSQL SQLSTATE 到 DomainError 的映射。仅处理数据库错误，其余由 sql-shared 兜底。

use ramag_domain::error::DomainError;
use ramag_infra_sql_shared::errors::{map_database_error, map_sqlx_common};

pub fn map_postgres_error(err: &sqlx::Error) -> DomainError {
    map_postgres_database_error(err).unwrap_or_else(|| map_sqlx_common(err))
}

pub fn map_postgres_database_error(err: &sqlx::Error) -> Option<DomainError> {
    map_database_error(err, postgres_error_friendly, |code, msg| {
        // SQLSTATE 类码前两位：08 表示连接错误，28 表示认证错误。
        match code.get(..2).unwrap_or("") {
            "08" | "28" => DomainError::ConnectionFailed(msg),
            _ => DomainError::QueryFailed(msg),
        }
    })
}

/// 将 SQLSTATE 转换为中文提示。
fn postgres_error_friendly(code: &str, raw: &str) -> String {
    match code {
        // 08 连接
        "08000" => format!("连接异常（{raw}）"),
        "08003" => format!("连接不存在（{raw}）"),
        "08006" => format!("连接失败（{raw}）"),
        "08001" => format!("无法建立连接，请检查主机、端口和防火墙：{raw}"),
        "08004" => format!("服务器拒绝连接（{raw}）"),

        // 23 完整性约束
        "23502" => format!("非空约束违反（NOT NULL）：{raw}"),
        "23503" => format!("外键约束违反：{raw}"),
        "23505" => format!("唯一键冲突：{raw}"),
        "23514" => format!("CHECK 约束违反：{raw}"),
        "23P01" => format!("EXCLUSION 约束违反：{raw}"),

        // 25 事务状态
        "25001" => format!("事务里只能跑一条语句（{raw}）"),
        "25P02" => format!("事务已中止，请回滚后重试：{raw}"),
        "25006" => format!("只读事务中不允许写：{raw}"),

        // 28 认证
        "28000" => format!("鉴权失败（{raw}）"),
        "28P01" => format!("用户名或密码错误：{raw}"),

        // 42 语法 / 权限
        "42000" => format!("语法或权限错误（{raw}）"),
        "42501" => format!("权限不足：{raw}"),
        "42601" => format!("SQL 语法错误：{raw}"),
        "42703" => format!("字段不存在：{raw}"),
        "42883" => format!("函数不存在：{raw}"),
        "42P01" => format!("表/视图不存在：{raw}"),
        "42P02" => format!("参数不存在：{raw}"),
        "42P07" => format!("对象已存在：{raw}"),

        // 53 资源
        "53100" => format!("磁盘满（{raw}）"),
        "53200" => format!("内存不足（{raw}）"),
        "53300" => format!("连接数已达上限（{raw}）"),

        "57014" => format!("查询被取消：{raw}"),

        "3D000" => format!("数据库不存在：{raw}"),
        "0A000" => format!("不支持的特性：{raw}"),

        // 22 数据异常
        "22001" => format!("字段值过长（{raw}）"),
        "22003" => format!("数值越界（{raw}）"),
        "22007" => format!("时间格式无效（{raw}）"),
        "22P02" => format!("文本表示无效（类型转换失败）：{raw}"),
        "22023" => format!("参数值无效（{raw}）"),

        _ => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_known_codes() {
        assert!(postgres_error_friendly("23505", "duplicate").contains("唯一键冲突"));
        assert!(postgres_error_friendly("42P01", "no table").contains("表/视图不存在"));
        assert!(postgres_error_friendly("28P01", "bad password").contains("用户名或密码"));
        assert!(postgres_error_friendly("57014", "canceled").contains("查询被取消"));
    }

    #[test]
    fn friendly_unknown_returns_raw() {
        assert_eq!(postgres_error_friendly("99999", "raw msg"), "raw msg");
    }
}
