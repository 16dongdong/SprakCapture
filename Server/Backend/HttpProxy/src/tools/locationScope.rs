use location_core::{
    LocationMatchOptions, LocationPattern, ResolvedLocation, locationMatches,
    validateLocationPattern,
};

use crate::tools::ToolError;

/// 在配置写入时校验全部 Location，避免运行中的数据面因持久化规则错误而产生不确定行为。
pub(crate) fn validateLocations(locations: &[LocationPattern]) -> Result<(), ToolError> {
    for (index, location) in locations.iter().enumerate() {
        validateLocationPattern(location)
            .map_err(|source| ToolError::InvalidLocationPattern { index, source })?;
    }
    Ok(())
}

/// 判断候选位置是否落入工具作用域；空规则列表按产品约定覆盖全部已解析位置。
pub(crate) fn matchesLocations(
    locations: &[LocationPattern],
    location: &ResolvedLocation,
) -> Result<bool, ToolError> {
    if locations.is_empty() {
        return Ok(true);
    }
    for pattern in locations {
        match locationMatches(pattern, location, LocationMatchOptions::default()) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(source) => return Err(ToolError::InvalidLocation { source }),
        }
    }
    Ok(false)
}
