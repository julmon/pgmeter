-- TPC-H Data Loading Script
-- Populates part, supplier, partsupp, customer, orders, lineitem.
--
-- Parallel model: `ids` is this job's slice of 1..scalefactor. Each id is one
-- self-contained "unit" worth one SF of data, with surrogate keys offset by
-- (unit-1)*per_unit_count so units never collide and every job loads in
-- parallel. Cross-table references (partsupp->part/supplier, orders->customer,
-- lineitem->orders/partsupp) are kept *within* the same unit, so the foreign
-- keys hold without any cross-unit coordination.

-- Per-unit sizing (one scalefactor's worth)
local PARTS_PER_UNIT = 200000
local SUPPLIERS_PER_UNIT = 10000
local CUSTOMERS_PER_UNIT = 150000
local ORDERS_PER_UNIT = 1500000
local PARTSUPPS_PER_PART = 4
local BATCH = 10000

-- Reference data
local SEGMENTS = {"AUTOMOBILE", "BUILDING", "FURNITURE", "HOUSEHOLD", "MACHINERY"}
local PRIORITIES = {"1-URGENT", "2-HIGH", "3-MEDIUM", "4-NOT SPECIFIED", "5-LOW"}
local SHIP_MODES = {"REG AIR", "AIR", "RAIL", "SHIP", "TRUCK", "MAIL", "FOB"}
local SHIP_INSTRUCT = {"DELIVER IN PERSON", "COLLECT COD", "NONE", "TAKE BACK RETURN"}
local CONTAINERS = {
    "SM CASE", "SM BOX", "SM PACK", "SM PKG", "SM JAR", "SM BAG", "SM CAN", "SM DRUM",
    "MED CASE", "MED BOX", "MED PACK", "MED PKG", "MED JAR", "MED BAG", "MED CAN", "MED DRUM",
    "LG CASE", "LG BOX", "LG PACK", "LG PKG", "LG JAR", "LG BAG", "LG CAN", "LG DRUM",
    "JUMBO CASE", "JUMBO BOX", "JUMBO PACK", "JUMBO PKG", "JUMBO JAR", "JUMBO BAG", "JUMBO CAN", "JUMBO DRUM",
    "WRAP CASE", "WRAP BOX", "WRAP PACK", "WRAP PKG", "WRAP JAR", "WRAP BAG", "WRAP CAN", "WRAP DRUM",
}
local T1 = {"STANDARD", "SMALL", "MEDIUM", "LARGE", "ECONOMY"}
local T2 = {"ANODIZED", "BURNISHED", "PLATED", "POLISHED", "BRUSHED"}
local T3 = {"TIN", "NICKEL", "BRASS", "STEEL", "COPPER", "IRON"}

local PART_WORDS = {
    "almond", "antique", "aquamarine", "azure", "beige", "bisque", "black", "blanched",
    "blue", "blush", "brown", "burlywood", "burnished", "chartreuse", "chiffon", "chocolate",
    "coral", "cornflower", "cornsilk", "cream", "cyan", "dark", "deep", "dim", "dodger",
    "drab", "firebrick", "floral", "forest", "frosted", "gainsboro", "ghost", "goldenrod",
    "green", "grey", "honeydew", "hot", "indian", "ivory", "khaki", "lace", "lavender",
    "lawn", "lemon", "light", "lime", "linen", "magenta", "maroon", "medium", "metallic",
    "midnight", "mint", "misty", "moccasin", "navajo", "navy", "olive", "orange", "orchid",
    "pale", "papaya", "peach", "peru", "pink", "plum", "powder", "puff", "purple", "red",
    "rose", "rosy", "royal", "saddle", "salmon", "sandy", "seashell", "sienna", "sky",
    "slate", "smoke", "snow", "spring", "steel", "tan", "thistle", "tomato", "turquoise",
    "violet", "wheat", "white", "yellow",
}

local function pick(t) return t[rand_int(1, #t)] end

local function gen_part_name()
    return pick(PART_WORDS) .. " " .. pick(PART_WORDS) .. " " .. pick(PART_WORDS)
        .. " " .. pick(PART_WORDS) .. " " .. pick(PART_WORDS)
end

local function gen_type()
    return pick(T1) .. " " .. pick(T2) .. " " .. pick(T3)
end

local function gen_phone(nation_key)
    return string.format("%d-%03d-%03d-%04d",
        10 + nation_key, rand_int(100, 998), rand_int(100, 998), rand_int(1000, 9998))
end

-- Pure-Lua date arithmetic (no SQL round-trips)
local function ymd_to_days(y, m, d)
    local a = math.floor((14 - m) / 12)
    local y2 = y + 4800 - a
    local m2 = m + 12 * a - 3
    return d + math.floor((153 * m2 + 2) / 5) + 365 * y2
        + math.floor(y2 / 4) - math.floor(y2 / 100) + math.floor(y2 / 400) - 32045
end

local function days_to_ymd(jd)
    local a = jd + 32044
    local b = math.floor((4 * a + 3) / 146097)
    local c = a - math.floor(146097 * b / 4)
    local d = math.floor((4 * c + 3) / 1461)
    local e = c - math.floor(1461 * d / 4)
    local m = math.floor((5 * e + 2) / 153)
    local day = e - math.floor((153 * m + 2) / 5) + 1
    local month = m + 3 - 12 * math.floor(m / 10)
    local year = 100 * b + d - 4800 + math.floor(m / 10)
    return year, month, day
end

local function rand_date(y1, y2)
    return string.format("%04d-%02d-%02d", rand_int(y1, y2), rand_int(1, 12), rand_int(1, 28))
end

-- Optimized: parse date once, add days via Julian arithmetic, format result
local function add_days_fast(y, m, d, n)
    local jd = ymd_to_days(y, m, d) + n
    local ny, nm, nd = days_to_ymd(jd)
    return string.format("%04d-%02d-%02d", ny, nm, nd), ny, nm, nd
end

-- Parse "YYYY-MM-DD" into y, m, d (avoid repeated string ops)
local function parse_date(ds)
    return tonumber(string.sub(ds, 1, 4)),
           tonumber(string.sub(ds, 6, 7)),
           tonumber(string.sub(ds, 9, 10))
end

-- Pre-allocate reusable buffer tables
local buf = {}
local obuf = {}
local lbuf = {}

-- Helper: flush n entries from buffer to COPY
local function flush_copy(sql, t, n)
    copy_start(sql)
    copy_write(table.concat(t, "", 1, n))
    copy_end()
end

-- ============================================================
-- One unit == one scalefactor's worth of data. Keys are offset
-- so jobs loading different units never collide.
-- ============================================================
for _, unit in ipairs(ids) do
    local part_off = (unit - 1) * PARTS_PER_UNIT
    local supp_off = (unit - 1) * SUPPLIERS_PER_UNIT
    local cust_off = (unit - 1) * CUSTOMERS_PER_UNIT
    local ord_off  = (unit - 1) * ORDERS_PER_UNIT

    -- --------------------------------------------------------
    -- 1. Part
    -- --------------------------------------------------------
    local n_batches = math.ceil(PARTS_PER_UNIT / BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * BATCH + 1
        local e = math.min(b * BATCH, PARTS_PER_UNIT)
        local cnt = e - s + 1
        local comments = rand_strings(cnt, 5, 22)
        local n = 0
        for lp = s, e do
            n = n + 1
            buf[n] = string.format("%d\t%s\t%s\t%s\t%s\t%d\t%s\t%.2f\t%s\n",
                part_off + lp, gen_part_name(),
                string.format("Manufacturer#%d", rand_int(1, 5)),
                string.format("Brand#%d%d", rand_int(1, 5), rand_int(1, 5)),
                gen_type(), rand_int(1, 50), pick(CONTAINERS),
                (90000 + (lp % 20001) + 100 * (lp % 1000)) / 100.0,
                comments[n])
        end
        flush_copy("COPY part FROM stdin", buf, n)
    end

    -- --------------------------------------------------------
    -- 2. Supplier
    -- --------------------------------------------------------
    n_batches = math.ceil(SUPPLIERS_PER_UNIT / 5000)
    for b = 1, n_batches do
        local s = (b - 1) * 5000 + 1
        local e = math.min(b * 5000, SUPPLIERS_PER_UNIT)
        local cnt = e - s + 1
        local addrs = rand_strings(cnt, 10, 40)
        local comments = rand_strings(cnt, 25, 100)
        local n = 0
        for ls = s, e do
            n = n + 1
            local gk = supp_off + ls
            local nk = rand_int(0, 24)
            buf[n] = string.format("%d\tSupplier#%09d\t%s\t%d\t%s\t%.2f\t%s\n",
                gk, gk, addrs[n], nk, gen_phone(nk),
                rand_float(-999.99, 9999.99), comments[n])
        end
        flush_copy("COPY supplier FROM stdin", buf, n)
    end

    -- --------------------------------------------------------
    -- 3. Partsupp (4 per part) -- same supplier-mapping formula as
    --    lineitem below, so (l_partkey, l_suppkey) always lands here.
    -- --------------------------------------------------------
    n_batches = math.ceil(PARTS_PER_UNIT / BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * BATCH + 1
        local e = math.min(b * BATCH, PARTS_PER_UNIT)
        local cnt = (e - s + 1) * PARTSUPPS_PER_PART
        local comments = rand_strings(cnt, 49, 198)
        local n = 0
        for lp = s, e do
            for i = 0, PARTSUPPS_PER_PART - 1 do
                n = n + 1
                local ps_sk = ((lp + i * (SUPPLIERS_PER_UNIT / 4 + (lp - 1) / SUPPLIERS_PER_UNIT)) % SUPPLIERS_PER_UNIT) + 1
                buf[n] = string.format("%d\t%d\t%d\t%.2f\t%s\n",
                    part_off + lp, supp_off + ps_sk, rand_int(1, 9999), rand_float(1.00, 1000.00), comments[n])
            end
        end
        flush_copy("COPY partsupp FROM stdin", buf, n)
    end

    -- --------------------------------------------------------
    -- 4. Customer
    -- --------------------------------------------------------
    n_batches = math.ceil(CUSTOMERS_PER_UNIT / BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * BATCH + 1
        local e = math.min(b * BATCH, CUSTOMERS_PER_UNIT)
        local cnt = e - s + 1
        local addrs = rand_strings(cnt, 10, 40)
        local comments = rand_strings(cnt, 29, 116)
        local n = 0
        for lc = s, e do
            n = n + 1
            local gk = cust_off + lc
            local nk = rand_int(0, 24)
            buf[n] = string.format("%d\tCustomer#%09d\t%s\t%d\t%s\t%.2f\t%s\t%s\n",
                gk, gk, addrs[n], nk, gen_phone(nk),
                rand_float(-999.99, 9999.99), pick(SEGMENTS), comments[n])
        end
        flush_copy("COPY customer FROM stdin", buf, n)
    end

    -- --------------------------------------------------------
    -- 5. Orders + Lineitem
    -- --------------------------------------------------------
    n_batches = math.ceil(ORDERS_PER_UNIT / BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * BATCH + 1
        local e = math.min(b * BATCH, ORDERS_PER_UNIT)
        local cnt = e - s + 1

        local o_comments = rand_strings(cnt, 19, 78)
        local l_comments = rand_strings(cnt * 7, 10, 43)  -- max 7 lines per order
        local lc_idx = 0

        local on = 0
        local ln = 0

        for lok = s, e do
            local o_ck = (lok % CUSTOMERS_PER_UNIT) + 1
            local o_date = rand_date(1992, 1998)
            local oy, om, od = parse_date(o_date)
            local o_priority = pick(PRIORITIES)
            local o_clerk = string.format("Clerk#%09d", rand_int(1, 1000))

            local n_lines = rand_int(1, 7)
            local o_totalprice = 0.0
            local has_returned = false

            for li = 1, n_lines do
                ln = ln + 1
                lc_idx = lc_idx + 1
                local l_pk = rand_int(1, PARTS_PER_UNIT)
                local supp_idx = rand_int(0, 3)
                local l_sk = ((l_pk + supp_idx * (SUPPLIERS_PER_UNIT / 4 + (l_pk - 1) / SUPPLIERS_PER_UNIT)) % SUPPLIERS_PER_UNIT) + 1
                local l_qty = rand_int(1, 50)
                local l_disc = rand_int(0, 10) / 100.0
                local l_tax = rand_int(0, 8) / 100.0
                local base_price = (90000 + (l_pk % 20001) + 100 * (l_pk % 1000)) / 100.0
                local l_extprice = base_price * l_qty
                o_totalprice = o_totalprice + l_extprice * (1 - l_disc) * (1 + l_tax)

                local l_shipdate, sy, sm, sd = add_days_fast(oy, om, od, rand_int(1, 121))
                local l_commitdate = add_days_fast(oy, om, od, rand_int(30, 90))
                local l_receiptdate = add_days_fast(sy, sm, sd, rand_int(1, 30))

                local l_rflag
                if l_receiptdate <= "1995-06-17" then
                    l_rflag = rand_int(0, 1) == 0 and "R" or "A"
                else
                    l_rflag = "N"
                end
                if l_rflag == "R" then has_returned = true end

                local l_lstatus = l_shipdate > "1995-06-17" and "O" or "F"

                lbuf[ln] = string.format(
                    "%d\t%d\t%d\t%d\t%d\t%.2f\t%.2f\t%.2f\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n",
                    ord_off + lok, part_off + l_pk, supp_off + l_sk, li, l_qty, l_extprice, l_disc, l_tax,
                    l_rflag, l_lstatus, l_shipdate, l_commitdate, l_receiptdate,
                    pick(SHIP_INSTRUCT), pick(SHIP_MODES), l_comments[lc_idx])
            end

            on = on + 1
            local o_status = has_returned and "P" or (o_date < "1995-06-17" and "F" or "O")
            obuf[on] = string.format("%d\t%d\t%s\t%.2f\t%s\t%s\t%s\t0\t%s\n",
                ord_off + lok, cust_off + o_ck, o_status, o_totalprice, o_date, o_priority, o_clerk,
                o_comments[on])
        end

        -- Flush orders
        copy_start("COPY orders FROM stdin")
        copy_write(table.concat(obuf, "", 1, on))
        copy_end()

        -- Flush lineitem
        copy_start("COPY lineitem FROM stdin")
        copy_write(table.concat(lbuf, "", 1, ln))
        copy_end()
    end
end
