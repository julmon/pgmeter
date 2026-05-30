-- TPCx-IoT Alert Query Transaction
-- Retrieves and acknowledges unacknowledged alerts

local substation_id = rand_int(min_id, max_id)

begin()

-- Get unacknowledged alerts
local rows = query(
    "SELECT a_id, a_sn_id, a_timestamp, a_type, a_value, a_threshold "
    .. "FROM alert WHERE a_ss_id = $1 AND a_acknowledged = false "
    .. "ORDER BY a_timestamp DESC LIMIT 20",
    substation_id
)

-- Acknowledge oldest alert if any exist
if #rows > 0 then
    local alert_id = rows[#rows]["a_id"]
    exec(
        "UPDATE alert SET a_acknowledged = true, a_acknowledged_at = NOW() WHERE a_id = $1",
        alert_id
    )
end

commit()
