-- PGM-DS Data Loading Script (Phases 1 & 2)
-- Populates the scale-factor-DEPENDENT dimension tables AND the fact tables.
--
-- Multi-job model: each id in `ids` is one "scale unit" (1 SF worth of data).
-- The executor hands every job a disjoint slice of 1..scalefactor, so this
-- script just loops over its own ids and loads one unit per id. That means
-- `init -j N` spreads the work across N threads whenever scalefactor >= N.
-- Per-unit surrogate keys are offset by (unit-1)*per_unit_count so the slices
-- never overlap and their union is byte-for-byte what a single job would load.
-- The small fixed dimensions (warehouse, store, ...) don't scale, so they load
-- exactly once, guarded on unit 1.
--
-- Dimension cardinalities (SF = scalefactor):
--   customer_address   50,000 * SF
--   customer          100,000 * SF
--   item               18,000 * SF
--   catalog_page       11,718         (kept fixed; grows ~log at larger scale)
--   promotion             300         (fixed)
--   store                  12         (fixed)
--   call_center             6         (fixed)
--   web_site               30         (fixed)
--   web_page               60         (fixed)
--   warehouse               5         (fixed)
--
-- Fact cardinalities (approximate, SF = scalefactor):
--   store_sales     2,880,000 * SF    catalog_sales   1,440,000 * SF
--   web_sales         720,000 * SF    inventory      11,700,000 * SF
--   *_returns       a coupled subset of the matching sales (see below)
--
-- Returns->sales referential coupling: each *_returns row is emitted inline
-- while generating its parent sale, reusing the sale's (item, ticket/order),
-- customer, and channel surrogate keys. This guarantees every return points at
-- a real sale line without reading anything back from the database.
--
-- Surrogate-key references (cdemo/hdemo/addr/date) point into the dimensions
-- created by pgmds_pre_load.lua. Date references use date_sk = Julian Day Number
-- (the same convention date_dim uses), within the years below.

local SF = scalefactor
-- Per-scale-unit cardinalities (one id == one unit).
local ADDR_PER, CUST_PER, ITEM_PER = 50000, 100000, 18000
-- Totals across all units; fact rows reference dimensions from every unit.
local N_ADDR = ADDR_PER * SF
local N_CUST = CUST_PER * SF
local N_ITEM = ITEM_PER * SF
local N_CDEMO = 1920800
local N_HDEMO = 7200
local BATCH = 10000

-- Date reference window (matches the populated end of date_dim's calendar).
local DATE_Y1, DATE_Y2 = 1998, 2003

-- Julian date arithmetic (shared convention with date_dim / the fact loader).
local function ymd_to_days(y, m, d)
    local a = math.floor((14 - m) / 12)
    local y2 = y + 4800 - a
    local m2 = m + 12 * a - 3
    return d + math.floor((153 * m2 + 2) / 5) + 365 * y2
        + math.floor(y2 / 4) - math.floor(y2 / 100) + math.floor(y2 / 400) - 32045
end

local function rand_date_sk(y1, y2)
    return ymd_to_days(rand_int(y1, y2), rand_int(1, 12), rand_int(1, 28))
end

local function rand_date_str(y1, y2)
    return string.format("%04d-%02d-%02d", rand_int(y1, y2), rand_int(1, 12), rand_int(1, 28))
end

local function pick(t) return t[rand_int(1, #t)] end

local function flush_copy(sql, t, n)
    if n == 0 then return end
    copy_start(sql)
    copy_write(table.concat(t, "", 1, n))
    copy_end()
end

local buf = {}

-- Reference vocabularies (compact, query-meaningful)
local STATES = {"AL","AK","AZ","AR","CA","CO","CT","DE","FL","GA","HI","ID",
    "IL","IN","IA","KS","KY","LA","ME","MD","MA","MI","MN","MS","MO","MT","NE",
    "NV","NH","NJ","NM","NY","NC","ND","OH","OK","OR","PA","RI","SC","SD","TN",
    "TX","UT","VT","VA","WA","WV","WI","WY"}
local STREET_TYPES = {"Road", "Street", "Avenue", "Lane", "Drive", "Court",
    "Blvd", "Way", "Circle", "Parkway", "Wy", "Ct", "Dr", "Ave", "Pkwy", "RD",
    "ST", "Ln", "Cir"}
local CITIES = {"Springfield", "Riverside", "Greenville", "Fairview", "Salem",
    "Franklin", "Clinton", "Georgetown", "Madison", "Arlington", "Centerville",
    "Lebanon", "Oak Grove", "Pleasant Hill", "Bridgeport", "Five Points",
    "Liberty", "Midway", "Hopewell", "Union"}
local COUNTIES = {"Williamson County", "Walker County", "Sumner County",
    "Mobile County", "Daviess County", "Orange County", "Marion County",
    "Greene County", "Jackson County", "Washington County"}
local LOC_TYPES = {"apartment", "condo", "single family"}
local FIRST_NAMES = {"James", "Mary", "John", "Patricia", "Robert", "Jennifer",
    "Michael", "Linda", "William", "Elizabeth", "David", "Barbara", "Richard",
    "Susan", "Joseph", "Jessica", "Thomas", "Sarah", "Charles", "Karen"}
local LAST_NAMES = {"Smith", "Johnson", "Williams", "Brown", "Jones", "Garcia",
    "Miller", "Davis", "Rodriguez", "Martinez", "Hernandez", "Lopez", "Wilson",
    "Anderson", "Thomas", "Taylor", "Moore", "Jackson", "Martin", "Lee"}
local SALUTATIONS = {"Mr.", "Mrs.", "Ms.", "Dr.", "Sir", "Miss"}
local COUNTRIES = {"United States", "Canada", "Mexico", "Germany", "France",
    "Japan", "Brazil", "India", "China", "Australia"}
local CATEGORIES = {"Women", "Men", "Children", "Shoes", "Music", "Jewelry",
    "Home", "Sports", "Books", "Electronics"}
-- Item classes per category, the lists the TPC-DS query templates filter on
-- (q53, q54, q63, q89 name classes such as "personal" or "reference").
local CLASSES_BY_CATEGORY = {
    {"dresses", "fragrances", "swimwear", "maternity"},
    {"accessories", "pants", "shirts", "sportswear"},
    {"infants", "newborn", "toddlers", "school-uniforms"},
    {"athletic", "kids", "mens", "womens"},
    {"classical", "country", "pop", "rock"},
    {"birdal", "costume", "diamonds", "estate", "gold", "loose stones",
     "mens watch", "pendants", "rings", "semi-precious", "womens watch",
     "consignment"},
    {"accent", "bathroom", "bedding", "blinds/shades", "curtains/drapes",
     "decor", "flatware", "furniture", "glassware", "kids", "lighting",
     "mattresses", "paint", "rugs", "tables", "wallpaper"},
    {"archery", "athletic shoes", "baseball", "basketball", "camping",
     "fishing", "fitness", "football", "golf", "guns", "hockey", "optics",
     "outdoor", "pools", "sailing", "tennis"},
    {"arts", "business", "computers", "cooking", "entertainments", "fiction",
     "history", "home repair", "mystery", "parenting", "reference", "romance",
     "science", "self-help", "sports", "travel"},
    {"audio", "automotive", "cameras", "camcorders", "disk drives",
     "dvd/vcr players", "karoke", "memory", "monitors", "musical", "personal",
     "portable", "scanners", "stereo", "televisions", "wireless"},
}
-- The TPC-DS syllables; names are made of two of them (1,000 manufacturers,
-- so that q3, q32, q37, q41, q82 and q92's i_manufact_id ranges select rows).
local SYLLABLES = {"ought", "able", "pri", "ese", "anti", "cally", "ation",
    "eing", "bar", "n st"}
local N_MANUFACT = 1000
local BRAND_ROOTS = {"amalg", "edu pack", "scholar", "importo", "exporti", "brand", "univ", "nameless", "corp"}
local function syllable_name(n)
    return SYLLABLES[(math.floor((n - 1) / 10) % 10) + 1] .. SYLLABLES[((n - 1) % 10) + 1]
end
local SIZES = {"small", "medium", "large", "extra large", "petite", "economy", "N/A"}
local UNITS = {"Each", "Dozen", "Case", "Box", "Pallet", "Carton", "Ounce",
    "Bunch", "Gross", "Cup", "Dram", "Lb", "Ton", "Tsp", "Gram", "Oz", "Unknown"}
local COLORS = {"almond", "antique", "aquamarine", "azure", "beige", "black",
    "blue", "brown", "coral", "cream", "cyan", "gainsboro", "goldenrod", "green",
    "grey", "ivory", "khaki", "lavender", "lime", "maroon", "navy", "olive",
    "orange", "orchid", "peach", "pink", "purple", "red", "rose", "royal",
    "salmon", "sienna", "sky", "smoke", "snow", "steel", "tan", "turquoise",
    "violet", "wheat", "white", "yellow"}
local DIVISIONS = {"able", "ation", "anti", "bar", "cally", "eing", "ese",
    "ought", "pri", "anti"}

-- A pool of 1,000 zip codes shared by addresses, stores and the other
-- outlets, so that a store's zip is a zip customers live in (q24 joins them).
local function zip() return string.format("%05d", 10000 + rand_int(0, 999) * 89) end
-- A few percent of some dimension keys are NULL in TPC-DS data (q76 counts the
-- sales with a missing store, ship customer or ship address). COPY text NULL.
local function maybe_null(sk) if rand_int(1, 100) <= 3 then return "\\N" end return tostring(sk) end
local function gmt() return string.format("%.2f", -1 * rand_int(5, 8)) end

-- Fixed dimension cardinalities the facts reference.
local N_STORE = 12
local N_CALL_CENTER = 6
local N_CATALOG_PAGE = 11718
local N_WEB_PAGE = 60
local N_WEB_SITE = 30
local N_SHIP_MODE = 20
local N_WAREHOUSE = 5
local N_PROMO = 300
local N_REASON = 35

-- Per-unit sales row targets and coupled-return rates (realistic ratios).
local SS_TARGET = 2880000
local CS_TARGET = 1440000
local WS_TARGET = 720000
local SR_RATE, CR_RATE, WR_RATE = 0.10, 0.14, 0.14
local SALE_Y1, SALE_Y2 = 1998, 2002

-- ###########################################################################
-- Per-unit generation. Each id handed to this job is one scale unit; an empty
-- ids list (a job given no units) makes this a no-op.
-- ###########################################################################
for _, unit in ipairs(ids) do
local addr_off = (unit - 1) * ADDR_PER
local cust_off = (unit - 1) * CUST_PER
local item_off = (unit - 1) * ITEM_PER

-- ============================================================
-- customer_address (50,000 per unit)
-- ============================================================
do
    local n_batches = math.ceil(ADDR_PER / BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * BATCH + 1
        local e = math.min(b * BATCH, ADDR_PER)
        local cnt = e - s + 1
        local streets = rand_strings(cnt, 5, 20)
        local n = 0
        for sk = s, e do
            n = n + 1
            local gk = addr_off + sk
            buf[n] = string.format(
                "%d\tAAAAAAAA%08d\t%d\t%s\t%s\tSuite %d\t%s\t%s\t%s\t%s\tUnited States\t%s\t%s\n",
                gk, gk, rand_int(1, 9999), streets[n], pick(STREET_TYPES),
                rand_int(1, 600), pick(CITIES), pick(COUNTIES), pick(STATES),
                zip(), gmt(), pick(LOC_TYPES))
        end
        flush_copy("COPY customer_address FROM stdin", buf, n)
    end
end

-- ============================================================
-- customer (100,000 per unit)
-- ============================================================
do
    local n_batches = math.ceil(CUST_PER / BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * BATCH + 1
        local e = math.min(b * BATCH, CUST_PER)
        local n = 0
        for sk = s, e do
            n = n + 1
            local gk = cust_off + sk
            local fn = pick(FIRST_NAMES)
            local ln = pick(LAST_NAMES)
            buf[n] = string.format(
                "%d\tAAAAAAAA%08d\t%d\t%d\t%d\t%d\t%d\t%s\t%s\t%s\t%s\t%d\t%d\t%d\t%s\t\t%s.%s@example.com\t%s\n",
                gk, gk,
                rand_int(1, N_CDEMO), rand_int(1, N_HDEMO), rand_int(1, N_ADDR),
                rand_date_sk(DATE_Y1, DATE_Y2), rand_date_sk(DATE_Y1, DATE_Y2),
                pick(SALUTATIONS), fn, ln,
                rand_int(0, 1) == 1 and "Y" or "N",
                rand_int(1, 28), rand_int(1, 12), rand_int(1924, 2000),
                pick(COUNTRIES), fn, ln,
                rand_date_str(2002, 2003))
        end
        flush_copy("COPY customer FROM stdin", buf, n)
    end
end

-- ============================================================
-- item (18,000 per unit)
-- ============================================================
do
    local n_batches = math.ceil(ITEM_PER / BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * BATCH + 1
        local e = math.min(b * BATCH, ITEM_PER)
        local cnt = e - s + 1
        local descs = rand_strings(cnt, 20, 120)
        local forms = rand_strings(cnt, 5, 20)
        local n = 0
        for sk = s, e do
            n = n + 1
            local gk = item_off + sk
            local cat_id = rand_int(1, #CATEGORIES)
            local classes = CLASSES_BY_CATEGORY[cat_id]
            local class_id = rand_int(1, #classes)
            -- Brand names follow the TPC-DS pattern: two roots and a number
            -- ("amalgimporto #1"), i_brand_id encodes them.
            local root1, root2, brand_n = rand_int(1, #BRAND_ROOTS), rand_int(1, #BRAND_ROOTS), rand_int(1, 16)
            local brand_id = root1 * 1000 + root2 * 100 + brand_n
            local brand = BRAND_ROOTS[root1] .. BRAND_ROOTS[root2] .. " #" .. brand_n
            local manu_id = rand_int(1, N_MANUFACT)
            buf[n] = string.format(
                "%d\tAAAAAAAA%08d\t1997-10-27\t2000-10-26\t%s\t%.2f\t%.2f\t%d\t%s\t%d\t%s\t%d\t%s\t%d\t%s\t%s\t%s\t%s\t%s\tUnknown\t%d\t%s\n",
                gk, gk, descs[n],
                rand_float(0.99, 99.99), rand_float(0.50, 80.00),
                brand_id, brand,
                class_id, classes[class_id],
                cat_id, CATEGORIES[cat_id],
                manu_id, syllable_name(manu_id),
                pick(SIZES), forms[n], pick(COLORS), pick(UNITS),
                rand_int(1, 100),
                syllable_name(rand_int(1, 100)) .. syllable_name(rand_int(1, 100)))
        end
        flush_copy("COPY item FROM stdin", buf, n)
    end
end

-- ------------------------------------------------------------
-- Fixed (non-scaling) dimensions: load exactly once, on unit 1.
-- ------------------------------------------------------------
if unit == 1 then

-- ============================================================
-- warehouse (5)
-- ============================================================
do
    local NAMES = {"Conventional childr", "Important issues liv",
        "Doors canno", "Bad cards must make.", "Powers will not get "}
    local n = 0
    for sk = 1, 5 do
        n = n + 1
        buf[n] = string.format(
            "%d\tAAAAAAAA%08d\t%s\t%d\t%d\t%s\t%s\tSuite %d\t%s\t%s\t%s\t%s\tUnited States\t%s\n",
            sk, sk, NAMES[sk], rand_int(50000, 999999),
            rand_int(1, 9999), pick(CITIES), pick(STREET_TYPES), rand_int(1, 600),
            pick(CITIES), pick(COUNTIES), pick(STATES), zip(), gmt())
    end
    flush_copy("COPY warehouse FROM stdin", buf, n)
end

-- ============================================================
-- store (12)
-- ============================================================
do
    local n = 0
    for sk = 1, 12 do
        n = n + 1
        buf[n] = string.format(
            "%d\tAAAAAAAA%08d\t1997-03-13\t2099-12-31\t%d\t%s\t%d\t%d\t8AM-4PM\t%s %s\t%d\t%s\tgeo desc\t%s %s\t%d\t%s\t%d\t%s\t%d\t%s\t%s\tSuite %d\t%s\t%s\t%s\t%s\tUnited States\t%s\t%.2f\n",
            sk, sk, rand_date_sk(2000, 2003), SYLLABLES[((sk - 1) % #SYLLABLES) + 1],
            rand_int(200, 300), rand_int(5000000, 9000000),
            pick(FIRST_NAMES), pick(LAST_NAMES),
            rand_int(1, 10), pick(DIVISIONS),
            pick(FIRST_NAMES), pick(LAST_NAMES),
            rand_int(1, 10), pick(DIVISIONS),
            rand_int(1, 10), pick(DIVISIONS),
            rand_int(1, 9999), pick(CITIES), pick(STREET_TYPES), rand_int(1, 600),
            pick(CITIES), pick(COUNTIES), pick(STATES), zip(), gmt(),
            rand_float(0.0, 0.12))
    end
    flush_copy("COPY store FROM stdin", buf, n)
end

-- ============================================================
-- call_center (6)
-- ============================================================
do
    local CLASSES_CC = {"large", "medium", "small"}
    local n = 0
    for sk = 1, 6 do
        n = n + 1
        buf[n] = string.format(
            "%d\tAAAAAAAA%08d\t1998-01-01\t2099-12-31\t%d\t%d\tCall Center %d\t%s\t%d\t%d\t8AM-4PM\t%s %s\t%d\tcls\tmkt desc\t%s %s\t%d\t%s\t%d\tDivision %d\t%d\t%s\t%s\tSuite %d\t%s\t%s\t%s\t%s\tUnited States\t%s\t%.2f\n",
            sk, sk, rand_date_sk(1998, 1999), rand_date_sk(1998, 1999), sk,
            pick(CLASSES_CC), rand_int(20, 500), rand_int(1000, 9000),
            pick(FIRST_NAMES), pick(LAST_NAMES), rand_int(1, 6),
            pick(FIRST_NAMES), pick(LAST_NAMES), rand_int(1, 6), pick(DIVISIONS),
            rand_int(1, 6), rand_int(1, 6),
            rand_int(1, 9999), pick(CITIES), pick(STREET_TYPES), rand_int(1, 600),
            pick(CITIES), pick(COUNTIES), pick(STATES), zip(), gmt(),
            rand_float(0.0, 0.12))
    end
    flush_copy("COPY call_center FROM stdin", buf, n)
end

-- ============================================================
-- web_site (30)
-- ============================================================
do
    local n = 0
    for sk = 1, 30 do
        n = n + 1
        buf[n] = string.format(
            "%d\tAAAAAAAA%08d\t1997-08-16\t2099-12-31\tsite_%d\t%d\t%d\t%s\t%s %s\t%d\tcls\tmkt desc\t%s %s\t%d\t%s\t%d\t%s\t%s\tSuite %d\t%s\t%s\t%s\t%s\tUnited States\t%s\t%.2f\n",
            sk, sk, sk, rand_date_sk(1997, 1998), rand_date_sk(2002, 2003),
            pick(DIVISIONS), pick(FIRST_NAMES), pick(LAST_NAMES), rand_int(1, 6),
            pick(FIRST_NAMES), pick(LAST_NAMES), rand_int(1, 6), SYLLABLES[((sk - 1) % #SYLLABLES) + 1],
            rand_int(1, 9999), pick(CITIES), pick(STREET_TYPES), rand_int(1, 600),
            pick(CITIES), pick(COUNTIES), pick(STATES), zip(), gmt(),
            rand_float(0.0, 0.12))
    end
    flush_copy("COPY web_site FROM stdin", buf, n)
end

-- ============================================================
-- web_page (60)
-- ============================================================
do
    local TYPES = {"welcome", "protected", "feedback", "general", "dynamic", "order", "ad"}
    local n = 0
    for sk = 1, 60 do
        n = n + 1
        buf[n] = string.format(
            "%d\tAAAAAAAA%08d\t1997-09-03\t2099-12-31\t%d\t%d\t%s\t%d\thttp://www.example.com/page%d\t%s\t%d\t%d\t%d\t%d\n",
            sk, sk, rand_date_sk(1997, 1998), rand_date_sk(2001, 2003),
            rand_int(0, 1) == 1 and "Y" or "N", rand_int(1, N_CUST), sk,
            pick(TYPES), rand_int(100, 8000), rand_int(2, 25),
            rand_int(1, 7), rand_int(1, 4))
    end
    flush_copy("COPY web_page FROM stdin", buf, n)
end

-- ============================================================
-- catalog_page (11,718)
-- ============================================================
do
    local DEPTS = {"DEPARTMENT", "ELECTRONICS", "HOME", "APPAREL", "SPORTS"}
    local TYPES = {"bi-annual", "monthly", "quarterly"}
    local N_CP = 11718
    local n_batches = math.ceil(N_CP / BATCH)
    for b = 1, n_batches do
        local s = (b - 1) * BATCH + 1
        local e = math.min(b * BATCH, N_CP)
        local cnt = e - s + 1
        local descs = rand_strings(cnt, 20, 90)
        local n = 0
        for sk = s, e do
            n = n + 1
            buf[n] = string.format(
                "%d\tAAAAAAAA%08d\t%d\t%d\t%s\t%d\t%d\t%s\t%s\n",
                sk, sk, rand_date_sk(2000, 2000), rand_date_sk(2001, 2001),
                pick(DEPTS), rand_int(1, 50), sk, descs[n], pick(TYPES))
        end
        flush_copy("COPY catalog_page FROM stdin", buf, n)
    end
end

-- ============================================================
-- promotion (300)
-- ============================================================
do
    local PURPOSES = {"Unknown", "promotion", "clearance"}
    local function yn() return rand_int(0, 1) == 1 and "Y" or "N" end
    local n = 0
    for sk = 1, 300 do
        n = n + 1
        local sd = rand_date_sk(2000, 2001)
        buf[n] = string.format(
            "%d\tAAAAAAAA%08d\t%d\t%d\t%d\t%.2f\t%d\tPromo %d\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\tdetails\t%s\t%s\n",
            sk, sk, sd, sd + rand_int(15, 90), rand_int(1, N_ITEM),
            rand_float(1.0, 5000.0), rand_int(1, 5000), sk,
            yn(), yn(), yn(), yn(), yn(), yn(), yn(), yn(),
            pick(PURPOSES), yn())
    end
    flush_copy("COPY promotion FROM stdin", buf, n)
end

end -- if unit == 1 (fixed dimensions)

-- ###########################################################################
-- Phase 2: Fact tables (per unit)
-- ###########################################################################

-- ============================================================
-- store_sales (2,880,000 per unit) + store_returns (~10% coupled)
-- ============================================================
do
    local ret = {}          -- store_returns line buffer
    local n, rn = 0, 0
    local ticket = (unit - 1) * SS_TARGET
    local rows = 0
    while rows < SS_TARGET do
        ticket = ticket + 1
        local sold_date = rand_date_sk(SALE_Y1, SALE_Y2)
        local sold_time = rand_int(0, 86399)
        local cust = rand_int(1, N_CUST)
        local cdemo = rand_int(1, N_CDEMO)
        local hdemo = rand_int(1, N_HDEMO)
        local addr = rand_int(1, N_ADDR)
        local store = rand_int(1, N_STORE)
        local lines = rand_int(1, 16)
        local seen = {}
        for _ = 1, lines do
            if rows >= SS_TARGET then break end
            local item
            repeat item = rand_int(1, N_ITEM) until not seen[item]
            seen[item] = true
            local q = rand_int(1, 100)
            local wc = rand_float(1.0, 100.0)
            local lp = wc * (1.0 + rand_float(0.0, 2.0))
            local sp = lp * (1.0 - rand_float(0.0, 1.0))
            local ext_list = lp * q
            local ext_sales = sp * q
            local ext_wholesale = wc * q
            local ext_disc = (lp - sp) * q
            local coupon = (rand_int(1, 10) <= 2) and ext_sales * rand_float(0.0, 0.5) or 0.0
            local tax_rate = rand_float(0.0, 0.10)
            local ext_tax = (ext_sales - coupon) * tax_rate
            local net_paid = ext_sales - coupon
            local net_paid_tax = net_paid + ext_tax
            local net_profit = net_paid - ext_wholesale
            n = n + 1
            buf[n] = string.format(
                "%d\t%d\t%d\t%d\t%d\t%d\t%s\t%s\t%d\t%d\t%d\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\n",
                sold_date, sold_time, item, cust, cdemo, hdemo, maybe_null(addr), maybe_null(store),
                rand_int(1, N_PROMO), ticket, q, wc, lp, sp, ext_disc, ext_sales,
                ext_wholesale, ext_list, ext_tax, coupon, net_paid, net_paid_tax, net_profit)
            rows = rows + 1
            if n >= BATCH then
                flush_copy("COPY store_sales FROM stdin", buf, n); n = 0
            end
            if rand_float(0.0, 1.0) < SR_RATE then
                local rq = rand_int(1, q)
                local r_amt = sp * rq
                local r_tax = r_amt * tax_rate
                local fee = rand_float(0.0, 50.0)
                local r_ship = rand_float(0.0, 50.0)
                local f1 = rand_float(0.0, 1.0)
                local cash = r_amt * f1
                local reversed = r_amt * rand_float(0.0, 1.0 - f1)
                local credit = r_amt - cash - reversed
                rn = rn + 1
                ret[rn] = string.format(
                    "%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\n",
                    sold_date + rand_int(1, 120), rand_int(0, 86399), item, cust, cdemo,
                    hdemo, addr, store, rand_int(1, N_REASON), ticket, rq, r_amt, r_tax,
                    r_amt + r_tax, fee, r_ship, cash, reversed, credit, r_amt + r_tax + r_ship + fee)
                if rn >= BATCH then
                    flush_copy("COPY store_returns FROM stdin", ret, rn); rn = 0
                end
            end
        end
    end
    flush_copy("COPY store_sales FROM stdin", buf, n)
    flush_copy("COPY store_returns FROM stdin", ret, rn)
end

-- ============================================================
-- catalog_sales (1,440,000 per unit) + catalog_returns (~14% coupled)
-- ============================================================
do
    local ret = {}
    local n, rn = 0, 0
    local order = (unit - 1) * CS_TARGET
    local rows = 0
    while rows < CS_TARGET do
        order = order + 1
        local sold_date = rand_date_sk(SALE_Y1, SALE_Y2)
        local sold_time = rand_int(0, 86399)
        local ship_date = sold_date + rand_int(1, 120)
        local bill_cust, bill_cdemo = rand_int(1, N_CUST), rand_int(1, N_CDEMO)
        local bill_hdemo, bill_addr = rand_int(1, N_HDEMO), rand_int(1, N_ADDR)
        local ship_cust, ship_cdemo = rand_int(1, N_CUST), rand_int(1, N_CDEMO)
        local ship_hdemo, ship_addr = rand_int(1, N_HDEMO), rand_int(1, N_ADDR)
        local call_center = rand_int(1, N_CALL_CENTER)
        local catalog_page = rand_int(1, N_CATALOG_PAGE)
        local ship_mode = rand_int(1, N_SHIP_MODE)
        local warehouse = rand_int(1, N_WAREHOUSE)
        local lines = rand_int(1, 16)
        local seen = {}
        for _ = 1, lines do
            if rows >= CS_TARGET then break end
            local item
            repeat item = rand_int(1, N_ITEM) until not seen[item]
            seen[item] = true
            local q = rand_int(1, 100)
            local wc = rand_float(1.0, 100.0)
            local lp = wc * (1.0 + rand_float(0.0, 2.0))
            local sp = lp * (1.0 - rand_float(0.0, 1.0))
            local ext_list = lp * q
            local ext_sales = sp * q
            local ext_wholesale = wc * q
            local ext_disc = (lp - sp) * q
            local coupon = (rand_int(1, 10) <= 2) and ext_sales * rand_float(0.0, 0.5) or 0.0
            local ext_ship = rand_float(1.0, 100.0)
            local tax_rate = rand_float(0.0, 0.10)
            local ext_tax = (ext_sales - coupon) * tax_rate
            local net_paid = ext_sales - coupon
            local net_paid_tax = net_paid + ext_tax
            local net_paid_ship = net_paid + ext_ship
            local net_paid_ship_tax = net_paid_ship + ext_tax
            local net_profit = net_paid - ext_wholesale
            n = n + 1
            buf[n] = string.format(
                "%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%s\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\n",
                sold_date, sold_time, ship_date, bill_cust, bill_cdemo, bill_hdemo,
                bill_addr, ship_cust, ship_cdemo, ship_hdemo, maybe_null(ship_addr), call_center,
                catalog_page, ship_mode, warehouse, item, rand_int(1, N_PROMO), order, q,
                wc, lp, sp, ext_disc, ext_sales, ext_wholesale, ext_list, ext_tax, coupon,
                ext_ship, net_paid, net_paid_tax, net_paid_ship, net_paid_ship_tax, net_profit)
            rows = rows + 1
            if n >= BATCH then
                flush_copy("COPY catalog_sales FROM stdin", buf, n); n = 0
            end
            if rand_float(0.0, 1.0) < CR_RATE then
                local rq = rand_int(1, q)
                local r_amt = sp * rq
                local r_tax = r_amt * tax_rate
                local fee = rand_float(0.0, 50.0)
                local r_ship = rand_float(0.0, 50.0)
                local f1 = rand_float(0.0, 1.0)
                local cash = r_amt * f1
                local reversed = r_amt * rand_float(0.0, 1.0 - f1)
                local credit = r_amt - cash - reversed
                rn = rn + 1
                ret[rn] = string.format(
                    "%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\n",
                    sold_date + rand_int(1, 120), rand_int(0, 86399), item, bill_cust,
                    bill_cdemo, bill_hdemo, bill_addr, bill_cust, bill_cdemo, bill_hdemo,
                    bill_addr, call_center, catalog_page, ship_mode, warehouse,
                    rand_int(1, N_REASON), order, rq, r_amt, r_tax, r_amt + r_tax, fee,
                    r_ship, cash, reversed, credit, r_amt + r_tax + r_ship + fee)
                if rn >= BATCH then
                    flush_copy("COPY catalog_returns FROM stdin", ret, rn); rn = 0
                end
            end
        end
    end
    flush_copy("COPY catalog_sales FROM stdin", buf, n)
    flush_copy("COPY catalog_returns FROM stdin", ret, rn)
end

-- ============================================================
-- web_sales (720,000 per unit) + web_returns (~14% coupled)
-- ============================================================
do
    local ret = {}
    local n, rn = 0, 0
    local order = (unit - 1) * WS_TARGET
    local rows = 0
    while rows < WS_TARGET do
        order = order + 1
        local sold_date = rand_date_sk(SALE_Y1, SALE_Y2)
        local sold_time = rand_int(0, 86399)
        local ship_date = sold_date + rand_int(1, 120)
        local bill_cust, bill_cdemo = rand_int(1, N_CUST), rand_int(1, N_CDEMO)
        local bill_hdemo, bill_addr = rand_int(1, N_HDEMO), rand_int(1, N_ADDR)
        local ship_cust, ship_cdemo = rand_int(1, N_CUST), rand_int(1, N_CDEMO)
        local ship_hdemo, ship_addr = rand_int(1, N_HDEMO), rand_int(1, N_ADDR)
        local web_page = rand_int(1, N_WEB_PAGE)
        local web_site = rand_int(1, N_WEB_SITE)
        local ship_mode = rand_int(1, N_SHIP_MODE)
        local warehouse = rand_int(1, N_WAREHOUSE)
        local lines = rand_int(1, 16)
        local seen = {}
        for _ = 1, lines do
            if rows >= WS_TARGET then break end
            local item
            repeat item = rand_int(1, N_ITEM) until not seen[item]
            seen[item] = true
            local q = rand_int(1, 100)
            local wc = rand_float(1.0, 100.0)
            local lp = wc * (1.0 + rand_float(0.0, 2.0))
            local sp = lp * (1.0 - rand_float(0.0, 1.0))
            local ext_list = lp * q
            local ext_sales = sp * q
            local ext_wholesale = wc * q
            local ext_disc = (lp - sp) * q
            local coupon = (rand_int(1, 10) <= 2) and ext_sales * rand_float(0.0, 0.5) or 0.0
            local ext_ship = rand_float(1.0, 100.0)
            local tax_rate = rand_float(0.0, 0.10)
            local ext_tax = (ext_sales - coupon) * tax_rate
            local net_paid = ext_sales - coupon
            local net_paid_tax = net_paid + ext_tax
            local net_paid_ship = net_paid + ext_ship
            local net_paid_ship_tax = net_paid_ship + ext_tax
            local net_profit = net_paid - ext_wholesale
            n = n + 1
            buf[n] = string.format(
                "%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%s\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\n",
                sold_date, sold_time, ship_date, item, bill_cust, bill_cdemo, bill_hdemo,
                bill_addr, maybe_null(ship_cust), ship_cdemo, ship_hdemo, ship_addr, web_page, web_site,
                ship_mode, warehouse, rand_int(1, N_PROMO), order, q, wc, lp, sp, ext_disc,
                ext_sales, ext_wholesale, ext_list, ext_tax, coupon, ext_ship, net_paid,
                net_paid_tax, net_paid_ship, net_paid_ship_tax, net_profit)
            rows = rows + 1
            if n >= BATCH then
                flush_copy("COPY web_sales FROM stdin", buf, n); n = 0
            end
            if rand_float(0.0, 1.0) < WR_RATE then
                local rq = rand_int(1, q)
                local r_amt = sp * rq
                local r_tax = r_amt * tax_rate
                local fee = rand_float(0.0, 50.0)
                local r_ship = rand_float(0.0, 50.0)
                local f1 = rand_float(0.0, 1.0)
                local cash = r_amt * f1
                local reversed = r_amt * rand_float(0.0, 1.0 - f1)
                local credit = r_amt - cash - reversed
                rn = rn + 1
                ret[rn] = string.format(
                    "%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\t%.2f\n",
                    sold_date + rand_int(1, 120), rand_int(0, 86399), item, bill_cust,
                    bill_cdemo, bill_hdemo, bill_addr, bill_cust, bill_cdemo, bill_hdemo,
                    bill_addr, web_page, rand_int(1, N_REASON), order, rq, r_amt, r_tax,
                    r_amt + r_tax, fee, r_ship, cash, reversed, credit, r_amt + r_tax + r_ship + fee)
                if rn >= BATCH then
                    flush_copy("COPY web_returns FROM stdin", ret, rn); rn = 0
                end
            end
        end
    end
    flush_copy("COPY web_sales FROM stdin", buf, n)
    flush_copy("COPY web_returns FROM stdin", ret, rn)
end

-- ============================================================
-- inventory (item x warehouse x weekly snapshot, ~11.7M per unit)
-- ============================================================
do
    local INV_WEEKS = 130
    local base = ymd_to_days(1998, 1, 1)
    local n = 0
    for w = 0, INV_WEEKS - 1 do
        local dsk = base + w * 7
        for item = item_off + 1, item_off + ITEM_PER do
            for wh = 1, N_WAREHOUSE do
                n = n + 1
                buf[n] = string.format("%d\t%d\t%d\t%d\n", dsk, item, wh, rand_int(0, 1000))
                if n >= BATCH then
                    flush_copy("COPY inventory FROM stdin", buf, n); n = 0
                end
            end
        end
    end
    flush_copy("COPY inventory FROM stdin", buf, n)
end

end -- for each unit in ids
