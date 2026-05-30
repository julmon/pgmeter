-- TPC-C Payment Transaction
-- Processes a customer payment

local warehouse_id = rand_int(min_id, max_id)
local district_id = rand_int(1, 10)
local x = rand_int(1, 100)
local y = rand_int(1, 100)

local c_d_id, c_w_id
if x <= 85 then
    c_d_id = district_id
    c_w_id = warehouse_id
else
    c_d_id = rand_int(1, 10)
    if (max_id - min_id) > 0 then
        c_w_id = warehouse_id
        while c_w_id == warehouse_id do
            c_w_id = rand_int(min_id, max_id)
        end
    else
        c_w_id = warehouse_id
    end
end

local c_id = rand_int(1, const.customers_per_district)
local h_amount = rand_float(1.00, 5000.00)

-- TPC-C last name generation (matches Rust implementation)
local syllables = {"BAR", "OUGHT", "ABLE", "PRIS", "PRES", "ESE", "ANTI", "CALLY", "ATION", "EING"}
local function gen_last(n)
    if n >= 1000 then n = rand_int(0, 999) end
    local s = string.format("%03d", n)
    return syllables[tonumber(string.sub(s, 1, 1)) + 1]
        .. syllables[tonumber(string.sub(s, 2, 2)) + 1]
        .. syllables[tonumber(string.sub(s, 3, 3)) + 1]
end

local c_last = ""
if y <= 60 then
    c_last = gen_last(rand_int(1, 1000))
end

begin_tx()

-- Update warehouse year-to-date
local w_rows = query(
    "UPDATE warehouse SET w_ytd = w_ytd + $1::NUMERIC WHERE w_id = $2 RETURNING w_name",
    h_amount, warehouse_id
)
local w_name = w_rows[1]["w_name"]

-- Update district year-to-date
local d_rows = query(
    "UPDATE district SET d_ytd = d_ytd + $1::NUMERIC WHERE d_w_id = $2 AND d_id = $3 RETURNING d_name",
    h_amount, warehouse_id, district_id
)
local d_name = d_rows[1]["d_name"]

-- Customer lookup
if y <= 60 then
    -- By last name
    local c_rows = query(
        "SELECT c_id FROM customer WHERE c_w_id = $1 AND c_d_id = $2 AND c_last = $3 ORDER BY c_first ASC",
        c_w_id, c_d_id, c_last
    )
    if #c_rows == 0 then
        rollback()
        error("Payment rollback: customer not found by last name")
    end
    local n = #c_rows
    c_id = c_rows[math.floor(n / 2) + 1]["c_id"]
end

-- Get customer credit status
local cust_rows = query(
    "SELECT c_credit FROM customer WHERE c_w_id = $1 AND c_d_id = $2 AND c_id = $3",
    c_w_id, c_d_id, c_id
)
local c_credit = cust_rows[1]["c_credit"]

-- Update customer based on credit type
if c_credit ~= nil and string.find(c_credit, "BC") then
    local pre_c_data = string.format("%d %d %d %d %d %.2f", c_id, c_d_id, c_w_id, district_id, warehouse_id, h_amount)
    exec(
        "UPDATE customer SET c_balance = c_balance - $1::NUMERIC, c_ytd_payment = c_ytd_payment + 1, c_data = substring($5||' '||c_data, 1, 500) WHERE c_id = $2 AND c_d_id = $3 AND c_w_id = $4",
        h_amount, c_id, c_d_id, c_w_id, pre_c_data
    )
else
    exec(
        "UPDATE customer SET c_balance = c_balance - $1::NUMERIC, c_ytd_payment = c_ytd_payment + 1 WHERE c_id = $2 AND c_d_id = $3 AND c_w_id = $4",
        h_amount, c_id, c_d_id, c_w_id
    )
end

-- Insert history
local h_data = string.sub((w_name or "") .. "    " .. (d_name or ""), 1, 24)
exec(
    "INSERT INTO history (h_c_id, h_c_d_id, h_c_w_id, h_d_id, h_w_id, h_date, h_amount, h_data) VALUES ($1, $2, $3, $4, $5, NOW(), $6, $7)",
    c_id, c_d_id, c_w_id, district_id, warehouse_id, h_amount, h_data
)

commit()
