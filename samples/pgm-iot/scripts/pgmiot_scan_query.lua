-- TPCx-IoT Scan Query Transaction
-- Time-range aggregation over recent sensor data

local substation_id = rand_int(min_id, max_id)

begin()

query(
    "SELECT m_sn_id, AVG(m_value), MIN(m_value), MAX(m_value), COUNT(*) "
    .. "FROM measurement WHERE m_ss_id = $1 AND m_timestamp >= NOW() - INTERVAL '1 hour' "
    .. "GROUP BY m_sn_id ORDER BY AVG(m_value) DESC LIMIT 10",
    substation_id
)

commit()
