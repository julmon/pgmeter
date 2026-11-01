-- PGM-C Per-Warehouse Data Loading Script
-- Populates warehouse, district, customer, history, stock, orders, new_order, order_line
-- for each warehouse ID in the `ids` table.

local DISTRICTS_PER_WH = const.districts_per_warehouse
local CUSTOMERS_PER_DIST = const.customers_per_district
local N_ITEMS = const.items
local BATCH_SIZE = 500

-- gen_last() comes from scripts/pgmc_common.lua (scripts.modules)

local function rand_state()
    local chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZ"
    local a = rand_int(1, 26)
    local b = rand_int(1, 26)
    return string.sub(chars, a, a) .. string.sub(chars, b, b)
end

-- TPC-C 4.3.2.7: a random 4-digit n-string followed by 11111.
local function rand_zip()
    return string.format("%04d11111", rand_int(0, 9999))
end

-- TPC-C 4.3.2.7: a 16-digit n-string. Two 8-digit draws: a single draw up to
-- 10^16 - 1 would exceed the 2^53 integers a Luau number holds exactly.
local function rand_phone()
    return string.format("%08d%08d", rand_int(0, 99999999), rand_int(0, 99999999))
end

-- TPC-C 4.3.3.1: a tax or discount drawn with 4 decimals, as "0.dddd";
-- max_ten_thousandths is 2000 for the taxes (0.2000) and 5000 for the
-- discount (0.5000).
local function rand_rate(max_ten_thousandths)
    return string.format("0.%04d", rand_int(0, max_ten_thousandths))
end

-- TPC-C 4.3.3.1: 10% of the strings carry "ORIGINAL" at a random position,
-- replacing 8 characters so the length stays within 26..50.
local function make_original(s)
    if rand_int(1, 100) <= 10 then
        local pos = rand_int(1, #s - 7)
        return string.sub(s, 1, pos - 1) .. "ORIGINAL" .. string.sub(s, pos + 8)
    end
    return s
end

local function now_str()
    -- Use a SQL query to get the current timestamp in a consistent format
    return query_one("SELECT to_char(NOW(), 'YYYY-MM-DD HH24:MI:SS')")
end

for _, wh_id in ipairs(ids) do
    local ts = now_str()

    -- 1. Warehouse
    copy_start("COPY warehouse FROM stdin")
    copy_write(string.format("%d\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t300000.00\n",
        wh_id, rand_string(6, 10), rand_string(10, 20), rand_string(10, 20),
        rand_string(10, 20), rand_state(), rand_zip(), rand_rate(2000)))
    copy_end()

    -- 2. District (10 per warehouse)
    copy_start("COPY district FROM stdin")
    for d_id = 1, DISTRICTS_PER_WH do
        copy_write(string.format("%d\t%d\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t30000.00\t3001\n",
            d_id, wh_id, rand_string(6, 10), rand_string(10, 20), rand_string(10, 20),
            rand_string(10, 20), rand_state(), rand_zip(), rand_rate(2000)))
    end
    copy_end()

    -- 3. Customer (3000 per district, 30000 per warehouse)
    local total_customers = DISTRICTS_PER_WH * CUSTOMERS_PER_DIST
    local n_batches = math.ceil(total_customers / BATCH_SIZE)
    local c_id = 1
    local d_id = 1

    for b = 1, n_batches do
        copy_start("COPY customer FROM stdin")
        local batch_end = math.min(b * BATCH_SIZE, total_customers)
        local batch_start = (b - 1) * BATCH_SIZE + 1

        for _ = batch_start, batch_end do
            local c_credit = "GC"
            if rand_int(1, 10) == 1 then c_credit = "BC" end

            copy_write(string.format(
                "%d\t%d\t%d\t%s\tOE\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t50000.00\t%s\t-10.00\t10.00\t1\t0\t%s\n",
                c_id, d_id, wh_id,
                rand_string(8, 16),         -- c_first
                gen_last(c_id - 1),         -- c_last (TPC-C 4.3.3.1)
                rand_string(10, 20),        -- c_street_1
                rand_string(10, 20),        -- c_street_2
                rand_string(10, 20),        -- c_city
                rand_state(),               -- c_state
                rand_zip(),                 -- c_zip
                rand_phone(),               -- c_phone
                ts,                         -- c_since
                c_credit,
                rand_rate(5000),            -- c_discount, 0.0000 .. 0.5000
                rand_string(300, 500)       -- c_data
            ))

            c_id = c_id + 1
            if c_id > CUSTOMERS_PER_DIST then
                d_id = d_id + 1
                c_id = 1
            end
        end
        copy_end()
    end

    -- 4. History (1 per customer = 30000 per warehouse)
    n_batches = math.ceil(total_customers / BATCH_SIZE)
    c_id = 1
    d_id = 1

    for b = 1, n_batches do
        copy_start("COPY history FROM stdin")
        local batch_end = math.min(b * BATCH_SIZE, total_customers)
        local batch_start = (b - 1) * BATCH_SIZE + 1

        for _ = batch_start, batch_end do
            copy_write(string.format("%d\t%d\t%d\t%d\t%d\t%s\t10.0\t%s\n",
                c_id, d_id, wh_id, d_id, wh_id, ts, rand_string(12, 24)))

            c_id = c_id + 1
            if c_id > CUSTOMERS_PER_DIST then
                d_id = d_id + 1
                c_id = 1
            end
        end
        copy_end()
    end

    -- 5. Stock (100000 per warehouse)
    n_batches = math.ceil(N_ITEMS / BATCH_SIZE)

    for b = 1, n_batches do
        copy_start("COPY stock FROM stdin")
        local batch_start = (b - 1) * BATCH_SIZE + 1
        local batch_end = math.min(b * BATCH_SIZE, N_ITEMS)

        for i_id = batch_start, batch_end do
            local s_data = make_original(rand_string(26, 50))
            copy_write(string.format(
                "%d\t%d\t%d\t0\t0\t0\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n",
                i_id, wh_id, rand_int(10, 100),
                rand_string(24, 24), rand_string(24, 24), rand_string(24, 24),
                rand_string(24, 24), rand_string(24, 24), rand_string(24, 24),
                rand_string(24, 24), rand_string(24, 24), rand_string(24, 24),
                rand_string(24, 24), s_data))
        end
        copy_end()
    end

    -- 6. Orders (3000 per district = 30000 per warehouse)
    -- TPC-C 4.3.3.1: O_C_ID is taken sequentially from a random permutation
    -- of the district's customers, and O_OL_CNT is drawn in 5..15 per order.
    -- The line counts are kept for the order_line loader below.
    local total_orders = DISTRICTS_PER_WH * CUSTOMERS_PER_DIST
    n_batches = math.ceil(total_orders / BATCH_SIZE)
    local o_id = 1
    d_id = 1
    local perm = {}
    local ol_cnts = {}

    for b = 1, n_batches do
        copy_start("COPY orders FROM stdin NULL AS ''")
        local batch_start = (b - 1) * BATCH_SIZE + 1
        local batch_end = math.min(b * BATCH_SIZE, total_orders)

        for _ = batch_start, batch_end do
            if o_id == 1 then
                -- A fresh permutation of 1..3000 for this district (Fisher-Yates)
                for i = 1, CUSTOMERS_PER_DIST do perm[i] = i end
                for i = CUSTOMERS_PER_DIST, 2, -1 do
                    local j = rand_int(1, i)
                    perm[i], perm[j] = perm[j], perm[i]
                end
            end
            local o_carrier_id = ""
            if o_id < 2101 then
                o_carrier_id = tostring(rand_int(1, 10))
            end
            local o_ol_cnt = rand_int(5, 15)
            ol_cnts[(d_id - 1) * CUSTOMERS_PER_DIST + o_id] = o_ol_cnt

            copy_write(string.format("%d\t%d\t%d\t%d\t%s\t%s\t%d\t1\n",
                o_id, d_id, wh_id, perm[o_id], ts, o_carrier_id, o_ol_cnt))

            o_id = o_id + 1
            if o_id > CUSTOMERS_PER_DIST then
                d_id = d_id + 1
                o_id = 1
            end
        end
        copy_end()
    end

    -- 7. New-Order (orders 2101-3000 per district = 9000 per warehouse)
    local total_new_orders = DISTRICTS_PER_WH * 900
    n_batches = math.ceil(total_new_orders / BATCH_SIZE)
    local no_id = 2101
    d_id = 1

    for b = 1, n_batches do
        copy_start("COPY new_order FROM stdin")
        local batch_start = (b - 1) * BATCH_SIZE + 1
        local batch_end = math.min(b * BATCH_SIZE, total_new_orders)

        for _ = batch_start, batch_end do
            copy_write(string.format("%d\t%d\t%d\n", no_id, d_id, wh_id))

            no_id = no_id + 1
            if no_id > 3000 then
                no_id = 2101
                d_id = d_id + 1
            end
        end
        copy_end()
    end

    -- 8. Order-Line (variable 5-15 per order, ~300000 per warehouse)
    -- Batch by groups of orders (50 orders per COPY batch)
    local OL_BATCH = 50
    n_batches = math.ceil(total_orders / OL_BATCH)
    o_id = 1
    d_id = 1

    for b = 1, n_batches do
        copy_start("COPY order_line FROM stdin NULL AS ''")
        local batch_start = (b - 1) * OL_BATCH + 1
        local batch_end = math.min(b * OL_BATCH, total_orders)

        for _ = batch_start, batch_end do
            local ol_cnt = ol_cnts[(d_id - 1) * CUSTOMERS_PER_DIST + o_id]

            for ol_num = 1, ol_cnt do
                local item_id = rand_int(1, N_ITEMS)
                local ol_delivery_d = ""
                local ol_amount = 0.00
                if o_id >= 2101 then
                    ol_amount = rand_float(0.01, 9999.99)
                else
                    ol_delivery_d = ts
                end

                copy_write(string.format("%d\t%d\t%d\t%d\t%d\t%d\t%s\t5\t%.2f\t%s\n",
                    o_id, d_id, wh_id, ol_num, item_id, wh_id,
                    ol_delivery_d, ol_amount, rand_string(24, 24)))
            end

            o_id = o_id + 1
            if o_id > CUSTOMERS_PER_DIST then
                d_id = d_id + 1
                o_id = 1
            end
        end
        copy_end()
    end
end
