-- TPCx-IoT Device Query Transaction
-- Single sensor data retrieval

local substation_id = rand_int(min_id, max_id)
local sensor_base = (substation_id - 1) * const.sensors_per_substation
local sensor_id = sensor_base + rand_int(1, const.sensors_per_substation)

begin_tx()

-- Get sensor details
query(
    "SELECT sn_id, sn_ss_id, sn_type, sn_unit, sn_min_threshold, sn_max_threshold, sn_status "
    .. "FROM sensor WHERE sn_id = $1",
    sensor_id
)

-- Get recent measurements
query(
    "SELECT m_id, m_timestamp, m_value, m_quality FROM measurement "
    .. "WHERE m_sn_id = $1 ORDER BY m_timestamp DESC LIMIT 100",
    sensor_id
)

commit()
