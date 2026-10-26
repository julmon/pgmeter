-- PGM-DS query workload: the TPC-DS queries, numbered as in the specification.
--
-- Loaded once per client as a shared module; defines the global dispatch table
-- Q[1]..Q[99]. Each transaction in spec.yaml runs `Q[n]()` through an inline
-- script. Multi-statement queries (14, 23, 24, 39) run both of their parts.
--
-- The SQL follows the TPC-DS query templates, written for PostgreSQL, with
-- the substitution parameters drawn at random within the template ranges on
-- every execution: years within the loaded sales window (1998..2002), dates
-- and month/week sequences within it, and categorical values from the
-- vocabularies the loader writes. Lists that TPC-DS draws from the data
-- (zip codes, counties, item ids) are sampled from the database once per
-- client and cached.
--
-- Seven queries are restructured for PostgreSQL with the same result rows:
-- Q4, Q11 and Q74 aggregate each channel and year separately instead of
-- self-joining one shared CTE (see year_total below), and Q1, Q6, Q30 and
-- Q81 join a per-group average instead of evaluating a correlated average
-- subquery for every row, which took up to a minute at scale factor 1.
--
-- PGM-DS is derived from TPC-DS; it is not an audited TPC benchmark and its
-- results are not comparable to official TPC-DS results.

-- ---------------------------------------------------------------------------
-- Parameter helpers
-- ---------------------------------------------------------------------------

local SALE_Y1, SALE_Y2 = 1998, 2002

local CATEGORIES = {"Women", "Men", "Children", "Shoes", "Music", "Jewelry",
    "Home", "Sports", "Books", "Electronics"}
local GENDER = {"M", "F"}
local MARITAL = {"M", "S", "D", "W", "U"}
local EDU = {"Primary", "Secondary", "College", "2 yr Degree",
    "4 yr Degree", "Advanced Degree", "Unknown"}
local BUY_POTENTIAL = {"0-500", "501-1000", "1001-5000", "5001-10000", ">10000", "Unknown"}
local COLORS = {"almond", "antique", "aquamarine", "azure", "beige", "black",
    "blue", "brown", "coral", "cream", "cyan", "gainsboro", "goldenrod", "green",
    "grey", "ivory", "khaki", "lavender", "lime", "maroon", "navy", "olive",
    "orange", "orchid", "peach", "pink", "purple", "red", "rose", "royal",
    "salmon", "sienna", "sky", "smoke", "snow", "steel", "tan", "turquoise",
    "violet", "wheat", "white", "yellow"}
local STATES = {"AL","AK","AZ","AR","CA","CO","CT","DE","FL","GA","HI","ID",
    "IL","IN","IA","KS","KY","LA","ME","MD","MA","MI","MN","MS","MO","MT","NE",
    "NV","NH","NJ","NM","NY","NC","ND","OH","OK","OR","PA","RI","SC","SD","TN",
    "TX","UT","VT","VA","WA","WV","WI","WY"}
local GMT_OFFSETS = {"-5.00", "-6.00", "-7.00", "-8.00"}

local function pick(t) return t[rand_int(1, #t)] end

-- n distinct picks from a list, as a quoted SQL list: 'a','b','c'
local function pick_n(t, n)
    local seen, out = {}, {}
    while #out < n and #out < #t do
        local v = pick(t)
        if not seen[v] then seen[v] = true; out[#out + 1] = "'" .. v .. "'" end
    end
    return table.concat(out, ",")
end

-- A year with `span` following years still inside the sales window.
local function year(span) return rand_int(SALE_Y1, SALE_Y2 - (span or 0)) end
local function moy() return rand_int(1, 12) end
local function qoy() return rand_int(1, 4) end

-- A date 'YYYY-MM-DD' in the sales window, leaving `days` of room after it.
local function date(days)
    local y, m = rand_int(SALE_Y1, SALE_Y2), rand_int(1, 12)
    if days and days > 120 and y == SALE_Y2 then y = SALE_Y2 - 1 end
    if days and days > 30 and y == SALE_Y2 and m > 10 then m = 10 end
    return string.format("%04d-%02d-%02d", y, m, rand_int(1, 28))
end

-- d_month_seq and d_week_seq as the loader computes them (date_dim).
local function month_seq(y, m) return (y - 1900) * 12 + (m - 1) end
local function week_seq_of(y, m, d)
    local function jd(yy, mm, dd)
        local a = math.floor((14 - mm) / 12)
        local y2 = yy + 4800 - a
        local m2 = mm + 12 * a - 3
        return dd + math.floor((153 * m2 + 2) / 5) + 365 * y2
            + math.floor(y2 / 4) - math.floor(y2 / 100) + math.floor(y2 / 400) - 32045
    end
    return math.floor((jd(y, m, d) - jd(1900, 1, 1)) / 7) + 1
end

-- Lists TPC-DS draws from the data, sampled once per client and cached.
local cache = {}
local function sample(key, sql)
    if not cache[key] then
        local rows = query(sql)
        local t = {}
        for i, r in ipairs(rows) do t[i] = r.v end
        cache[key] = t
    end
    return cache[key]
end
local function zips(n)
    return pick_n(sample("zip", "SELECT DISTINCT substr(ca_zip, 1, 5) AS v FROM customer_address LIMIT 2000"), n)
end
local function counties(n)
    return pick_n(sample("county", "SELECT DISTINCT ca_county AS v FROM customer_address"), n)
end
local function cities(n)
    return pick_n(sample("city", "SELECT DISTINCT ca_city AS v FROM customer_address"), n)
end
local function store_state()
    return pick(sample("sstate", "SELECT DISTINCT s_state AS v FROM store"))
end
local function warehouse_state()
    return pick(sample("wstate", "SELECT DISTINCT w_state AS v FROM warehouse"))
end
local function item_ids(n)
    return pick_n(sample("item", "SELECT i_item_id AS v FROM item ORDER BY i_item_sk LIMIT 1000"), n)
end
local function max_item_sk()
    return sample("maxitem", "SELECT max(i_item_sk) AS v FROM item")[1]
end

local function run(sql) exec(sql) end

-- Per-customer yearly totals of one channel, the building block of Q4, Q11 and
-- Q74. The templates write it as one shared year_total CTE over every channel
-- and year that is then self-joined four or six times with sale_type and year
-- filters; PostgreSQL has no statistics for a materialized CTE and chose plans
-- that ran for hours at scale factor 1. One aggregation per channel and year
-- gives the same rows and lets the planner see the base tables.
local YT = {
    s = {"store_sales", "ss_customer_sk", "ss_sold_date_sk", "ss_ext_list_price", "ss_ext_wholesale_cost", "ss_ext_discount_amt", "ss_ext_sales_price", "ss_net_paid"},
    c = {"catalog_sales", "cs_bill_customer_sk", "cs_sold_date_sk", "cs_ext_list_price", "cs_ext_wholesale_cost", "cs_ext_discount_amt", "cs_ext_sales_price", "cs_net_paid"},
    w = {"web_sales", "ws_bill_customer_sk", "ws_sold_date_sk", "ws_ext_list_price", "ws_ext_wholesale_cost", "ws_ext_discount_amt", "ws_ext_sales_price", "ws_net_paid"},
}
local function year_total(alias, channel, yr, measure)
    local t = YT[channel]
    local expr = ({
        q4 = string.format("((%s - %s - %s) + %s) / 2", t[4], t[5], t[6], t[7]),
        q11 = string.format("%s - %s", t[4], t[6]),
        q74 = t[8],
    })[measure]
    return string.format([[
       (SELECT c_customer_id customer_id, c_first_name customer_first_name, c_last_name customer_last_name,
               c_preferred_cust_flag customer_preferred_cust_flag, sum(%s) year_total
          FROM customer, %s, date_dim
         WHERE c_customer_sk = %s AND %s = d_date_sk AND d_year = %d
         GROUP BY c_customer_id, c_first_name, c_last_name, c_preferred_cust_flag) %s]],
        expr, t[1], t[2], t[3], yr, alias)
end


Q = {}

-- ---------------------------------------------------------------------------
-- Q1: customers whose store returns exceed 1.2 times the average of their store.
-- ---------------------------------------------------------------------------
Q[1] = function()
    run(string.format([[
WITH customer_total_return AS (
    SELECT sr_customer_sk AS ctr_customer_sk, sr_store_sk AS ctr_store_sk,
           sum(sr_return_amt) AS ctr_total_return
      FROM store_returns, date_dim
     WHERE sr_returned_date_sk = d_date_sk AND d_year = %d
     GROUP BY sr_customer_sk, sr_store_sk)
SELECT c_customer_id
  FROM customer_total_return ctr1, store, customer,
       (SELECT ctr_store_sk, avg(ctr_total_return) * 1.2 AS store_avg
          FROM customer_total_return GROUP BY ctr_store_sk) ctr2
 WHERE ctr1.ctr_store_sk = ctr2.ctr_store_sk AND ctr1.ctr_total_return > ctr2.store_avg
   AND s_store_sk = ctr1.ctr_store_sk AND s_state = '%s'
   AND ctr1.ctr_customer_sk = c_customer_sk
 ORDER BY c_customer_id LIMIT 100]], year(), store_state()))
end

-- Q2: web and catalog sales per day of the week, one year against the next.
Q[2] = function()
    local y = year(1)
    run(string.format([[
WITH wscs AS (
    SELECT sold_date_sk, sales_price
      FROM (SELECT ws_sold_date_sk sold_date_sk, ws_ext_sales_price sales_price FROM web_sales
            UNION ALL
            SELECT cs_sold_date_sk sold_date_sk, cs_ext_sales_price sales_price FROM catalog_sales) x),
wswscs AS (
    SELECT d_week_seq,
           sum(CASE WHEN d_day_name = 'Sunday'    THEN sales_price END) sun_sales,
           sum(CASE WHEN d_day_name = 'Monday'    THEN sales_price END) mon_sales,
           sum(CASE WHEN d_day_name = 'Tuesday'   THEN sales_price END) tue_sales,
           sum(CASE WHEN d_day_name = 'Wednesday' THEN sales_price END) wed_sales,
           sum(CASE WHEN d_day_name = 'Thursday'  THEN sales_price END) thu_sales,
           sum(CASE WHEN d_day_name = 'Friday'    THEN sales_price END) fri_sales,
           sum(CASE WHEN d_day_name = 'Saturday'  THEN sales_price END) sat_sales
      FROM wscs, date_dim
     WHERE d_date_sk = sold_date_sk
     GROUP BY d_week_seq)
SELECT d_week_seq1,
       round(sun_sales1 / sun_sales2, 2), round(mon_sales1 / mon_sales2, 2),
       round(tue_sales1 / tue_sales2, 2), round(wed_sales1 / wed_sales2, 2),
       round(thu_sales1 / thu_sales2, 2), round(fri_sales1 / fri_sales2, 2),
       round(sat_sales1 / sat_sales2, 2)
  FROM (SELECT wswscs.d_week_seq d_week_seq1, sun_sales sun_sales1, mon_sales mon_sales1,
               tue_sales tue_sales1, wed_sales wed_sales1, thu_sales thu_sales1,
               fri_sales fri_sales1, sat_sales sat_sales1
          FROM wswscs, date_dim
         WHERE date_dim.d_week_seq = wswscs.d_week_seq AND d_year = %d) y,
       (SELECT wswscs.d_week_seq d_week_seq2, sun_sales sun_sales2, mon_sales mon_sales2,
               tue_sales tue_sales2, wed_sales wed_sales2, thu_sales thu_sales2,
               fri_sales fri_sales2, sat_sales sat_sales2
          FROM wswscs, date_dim
         WHERE date_dim.d_week_seq = wswscs.d_week_seq AND d_year = %d) z
 WHERE d_week_seq1 = d_week_seq2 - 53
 ORDER BY d_week_seq1]], y, y + 1))
end

-- Q3: store sales by brand for one manufacturer in a month.
Q[3] = function()
    run(string.format([[
SELECT dt.d_year, item.i_brand_id brand_id, item.i_brand brand, sum(ss_ext_sales_price) sum_agg
  FROM date_dim dt, store_sales, item
 WHERE dt.d_date_sk = store_sales.ss_sold_date_sk AND store_sales.ss_item_sk = item.i_item_sk
   AND item.i_manufact_id = %d AND dt.d_moy = %d
 GROUP BY dt.d_year, item.i_brand, item.i_brand_id
 ORDER BY dt.d_year, sum_agg DESC, brand_id LIMIT 100]], rand_int(1, 1000), rand_int(11, 12)))
end

-- Q4: customers whose catalog growth beats both their store and web growth.
Q[4] = function()
    local y = year(1)
    run(string.format([[
SELECT t_s_secyear.customer_id, t_s_secyear.customer_first_name, t_s_secyear.customer_last_name,
       t_s_secyear.customer_preferred_cust_flag
  FROM %s,
%s,
%s,
%s,
%s,
%s
 WHERE t_s_secyear.customer_id = t_s_firstyear.customer_id
   AND t_s_firstyear.customer_id = t_c_secyear.customer_id
   AND t_s_firstyear.customer_id = t_c_firstyear.customer_id
   AND t_s_firstyear.customer_id = t_w_firstyear.customer_id
   AND t_s_firstyear.customer_id = t_w_secyear.customer_id
   AND t_s_firstyear.year_total > 0 AND t_c_firstyear.year_total > 0 AND t_w_firstyear.year_total > 0
   AND CASE WHEN t_c_firstyear.year_total > 0 THEN t_c_secyear.year_total / t_c_firstyear.year_total END
       > CASE WHEN t_s_firstyear.year_total > 0 THEN t_s_secyear.year_total / t_s_firstyear.year_total END
   AND CASE WHEN t_c_firstyear.year_total > 0 THEN t_c_secyear.year_total / t_c_firstyear.year_total END
       > CASE WHEN t_w_firstyear.year_total > 0 THEN t_w_secyear.year_total / t_w_firstyear.year_total END
 ORDER BY t_s_secyear.customer_id, t_s_secyear.customer_first_name, t_s_secyear.customer_last_name,
          t_s_secyear.customer_preferred_cust_flag
 LIMIT 100]],
        year_total("t_s_firstyear", "s", y, "q4"), year_total("t_s_secyear", "s", y + 1, "q4"),
        year_total("t_c_firstyear", "c", y, "q4"), year_total("t_c_secyear", "c", y + 1, "q4"),
        year_total("t_w_firstyear", "w", y, "q4"), year_total("t_w_secyear", "w", y + 1, "q4")))
end

-- Q5: sales, returns and profit by channel over 14 days, with rollup.
Q[5] = function()
    local d = date(14)
    run(string.format([[
WITH ssr AS (
    SELECT s_store_id, sum(sales_price) AS sales, sum(profit) AS profit,
           sum(return_amt) AS returns, sum(net_loss) AS profit_loss
      FROM (SELECT ss_store_sk AS store_sk, ss_sold_date_sk AS date_sk, ss_ext_sales_price AS sales_price,
                   ss_net_profit AS profit, cast(0 AS decimal(7,2)) AS return_amt, cast(0 AS decimal(7,2)) AS net_loss
              FROM store_sales
            UNION ALL
            SELECT sr_store_sk, sr_returned_date_sk, cast(0 AS decimal(7,2)), cast(0 AS decimal(7,2)),
                   sr_return_amt, sr_net_loss
              FROM store_returns) salesreturns, date_dim, store
     WHERE date_sk = d_date_sk AND d_date BETWEEN date '%s' AND date '%s' + 14
       AND store_sk = s_store_sk
     GROUP BY s_store_id),
csr AS (
    SELECT cp_catalog_page_id, sum(sales_price) AS sales, sum(profit) AS profit,
           sum(return_amt) AS returns, sum(net_loss) AS profit_loss
      FROM (SELECT cs_catalog_page_sk AS page_sk, cs_sold_date_sk AS date_sk, cs_ext_sales_price AS sales_price,
                   cs_net_profit AS profit, cast(0 AS decimal(7,2)) AS return_amt, cast(0 AS decimal(7,2)) AS net_loss
              FROM catalog_sales
            UNION ALL
            SELECT cr_catalog_page_sk, cr_returned_date_sk, cast(0 AS decimal(7,2)), cast(0 AS decimal(7,2)),
                   cr_return_amount, cr_net_loss
              FROM catalog_returns) salesreturns, date_dim, catalog_page
     WHERE date_sk = d_date_sk AND d_date BETWEEN date '%s' AND date '%s' + 14
       AND page_sk = cp_catalog_page_sk
     GROUP BY cp_catalog_page_id),
wsr AS (
    SELECT web_site_id, sum(sales_price) AS sales, sum(profit) AS profit,
           sum(return_amt) AS returns, sum(net_loss) AS profit_loss
      FROM (SELECT ws_web_site_sk AS wsr_web_site_sk, ws_sold_date_sk AS date_sk, ws_ext_sales_price AS sales_price,
                   ws_net_profit AS profit, cast(0 AS decimal(7,2)) AS return_amt, cast(0 AS decimal(7,2)) AS net_loss
              FROM web_sales
            UNION ALL
            SELECT ws_web_site_sk, wr_returned_date_sk, cast(0 AS decimal(7,2)), cast(0 AS decimal(7,2)),
                   wr_return_amt, wr_net_loss
              FROM web_returns LEFT OUTER JOIN web_sales
                ON (wr_item_sk = ws_item_sk AND wr_order_number = ws_order_number)) salesreturns, date_dim, web_site
     WHERE date_sk = d_date_sk AND d_date BETWEEN date '%s' AND date '%s' + 14
       AND wsr_web_site_sk = web_site_sk
     GROUP BY web_site_id)
SELECT channel, id, sum(sales) AS sales, sum(returns) AS returns, sum(profit) AS profit
  FROM (SELECT 'store channel' AS channel, 'store' || s_store_id AS id, sales, returns,
               (profit - profit_loss) AS profit FROM ssr
        UNION ALL
        SELECT 'catalog channel', 'catalog_page' || cp_catalog_page_id, sales, returns,
               (profit - profit_loss) FROM csr
        UNION ALL
        SELECT 'web channel', 'web_site' || web_site_id, sales, returns,
               (profit - profit_loss) FROM wsr) x
 GROUP BY ROLLUP (channel, id)
 ORDER BY channel, id LIMIT 100]], d, d, d, d, d, d))
end

-- Q6: states with at least ten customers buying items priced 20% above their category average.
Q[6] = function()
    run(string.format([[
SELECT a.ca_state state, count(*) cnt
  FROM customer_address a, customer c, store_sales s, date_dim d, item i,
       (SELECT i_category, avg(i_current_price) AS avg_price FROM item GROUP BY i_category) j
 WHERE a.ca_address_sk = c.c_current_addr_sk AND c.c_customer_sk = s.ss_customer_sk
   AND s.ss_sold_date_sk = d.d_date_sk AND s.ss_item_sk = i.i_item_sk
   AND d.d_month_seq = (SELECT DISTINCT d_month_seq FROM date_dim WHERE d_year = %d AND d_moy = %d)
   AND i.i_category = j.i_category AND i.i_current_price > 1.2 * j.avg_price
 GROUP BY a.ca_state HAVING count(*) >= 10
 ORDER BY cnt, a.ca_state LIMIT 100]], year(), moy()))
end

-- Q7: average quantity, list price, coupon and sales price by item for a demographic and promotion.
Q[7] = function()
    run(string.format([[
SELECT i_item_id, avg(ss_quantity) agg1, avg(ss_list_price) agg2, avg(ss_coupon_amt) agg3, avg(ss_sales_price) agg4
  FROM store_sales, customer_demographics, date_dim, item, promotion
 WHERE ss_sold_date_sk = d_date_sk AND ss_item_sk = i_item_sk AND ss_cdemo_sk = cd_demo_sk
   AND ss_promo_sk = p_promo_sk AND cd_gender = '%s' AND cd_marital_status = '%s'
   AND cd_education_status = '%s' AND (p_channel_email = 'N' OR p_channel_event = 'N') AND d_year = %d
 GROUP BY i_item_id ORDER BY i_item_id LIMIT 100]], pick(GENDER), pick(MARITAL), pick(EDU), year()))
end

-- Q8: store net profit for the stores in zip areas with at least ten preferred customers.
Q[8] = function()
    run(string.format([[
SELECT s_store_name, sum(ss_net_profit)
  FROM store_sales, date_dim, store,
       (SELECT ca_zip
          FROM (SELECT substr(ca_zip, 1, 5) ca_zip FROM customer_address WHERE substr(ca_zip, 1, 5) IN (%s)
                INTERSECT
                SELECT ca_zip
                  FROM (SELECT substr(ca_zip, 1, 5) ca_zip, count(*) cnt
                          FROM customer_address, customer
                         WHERE ca_address_sk = c_current_addr_sk AND c_preferred_cust_flag = 'Y'
                         GROUP BY ca_zip HAVING count(*) > 10) a1) a2) v1
 WHERE ss_store_sk = s_store_sk AND ss_sold_date_sk = d_date_sk AND d_qoy = %d AND d_year = %d
   AND substr(s_zip, 1, 2) = substr(v1.ca_zip, 1, 2)
 GROUP BY s_store_name ORDER BY s_store_name LIMIT 100]], zips(400), qoy(), year()))
end

-- Q9: bucketed statistics of store sales by quantity ranges.
Q[9] = function()
    local function b(lo, hi, threshold)
        return string.format(
            "CASE WHEN (SELECT count(*) FROM store_sales WHERE ss_quantity BETWEEN %d AND %d) > %d "
            .. "THEN (SELECT avg(ss_ext_discount_amt) FROM store_sales WHERE ss_quantity BETWEEN %d AND %d) "
            .. "ELSE (SELECT avg(ss_net_paid) FROM store_sales WHERE ss_quantity BETWEEN %d AND %d) END bucket%d",
            lo, hi, threshold, lo, hi, lo, hi, lo // 20 + 1)
    end
    run("SELECT " .. b(1, 20, rand_int(1, 1000000) * 10) .. ", " .. b(21, 40, rand_int(1, 1000000) * 10) .. ", "
        .. b(41, 60, rand_int(1, 1000000) * 10) .. ", " .. b(61, 80, rand_int(1, 1000000) * 10) .. ", "
        .. b(81, 100, rand_int(1, 1000000) * 10) .. " FROM reason WHERE r_reason_sk = 1")
end

-- Q10: demographics of the customers of five counties active in store and web or catalog over three months.
Q[10] = function()
    local y, m = year(), rand_int(1, 9)
    run(string.format([[
SELECT cd_gender, cd_marital_status, cd_education_status, count(*) cnt1, cd_purchase_estimate, count(*) cnt2,
       cd_credit_rating, count(*) cnt3, cd_dep_count, count(*) cnt4, cd_dep_employed_count, count(*) cnt5,
       cd_dep_college_count, count(*) cnt6
  FROM customer c, customer_address ca, customer_demographics
 WHERE c.c_current_addr_sk = ca.ca_address_sk AND ca_county IN (%s) AND cd_demo_sk = c.c_current_cdemo_sk
   AND EXISTS (SELECT * FROM store_sales, date_dim
                WHERE c.c_customer_sk = ss_customer_sk AND ss_sold_date_sk = d_date_sk
                  AND d_year = %d AND d_moy BETWEEN %d AND %d)
   AND (EXISTS (SELECT * FROM web_sales, date_dim
                 WHERE c.c_customer_sk = ws_bill_customer_sk AND ws_sold_date_sk = d_date_sk
                   AND d_year = %d AND d_moy BETWEEN %d AND %d)
        OR EXISTS (SELECT * FROM catalog_sales, date_dim
                    WHERE c.c_customer_sk = cs_ship_customer_sk AND cs_sold_date_sk = d_date_sk
                      AND d_year = %d AND d_moy BETWEEN %d AND %d))
 GROUP BY cd_gender, cd_marital_status, cd_education_status, cd_purchase_estimate, cd_credit_rating,
          cd_dep_count, cd_dep_employed_count, cd_dep_college_count
 ORDER BY cd_gender, cd_marital_status, cd_education_status, cd_purchase_estimate, cd_credit_rating,
          cd_dep_count, cd_dep_employed_count, cd_dep_college_count
 LIMIT 100]], counties(5), y, m, m + 3, y, m, m + 3, y, m, m + 3))
end

-- Q11: customers whose web growth beats their store growth, year over year.
Q[11] = function()
    local y = year(1)
    run(string.format([[
SELECT t_s_secyear.customer_id, t_s_secyear.customer_first_name, t_s_secyear.customer_last_name,
       t_s_secyear.customer_preferred_cust_flag
  FROM %s,
%s,
%s,
%s
 WHERE t_s_secyear.customer_id = t_s_firstyear.customer_id
   AND t_s_firstyear.customer_id = t_w_secyear.customer_id
   AND t_s_firstyear.customer_id = t_w_firstyear.customer_id
   AND t_s_firstyear.year_total > 0 AND t_w_firstyear.year_total > 0
   AND CASE WHEN t_w_firstyear.year_total > 0 THEN t_w_secyear.year_total / t_w_firstyear.year_total ELSE 0.0 END
       > CASE WHEN t_s_firstyear.year_total > 0 THEN t_s_secyear.year_total / t_s_firstyear.year_total ELSE 0.0 END
 ORDER BY t_s_secyear.customer_id, t_s_secyear.customer_first_name, t_s_secyear.customer_last_name,
          t_s_secyear.customer_preferred_cust_flag
 LIMIT 100]],
        year_total("t_s_firstyear", "s", y, "q11"), year_total("t_s_secyear", "s", y + 1, "q11"),
        year_total("t_w_firstyear", "w", y, "q11"), year_total("t_w_secyear", "w", y + 1, "q11")))
end

-- Q12: web sales by item over 30 days for three categories, with the share of its class.
Q[12] = function()
    local d = date(30)
    run(string.format([[
SELECT i_item_id, i_item_desc, i_category, i_class, i_current_price,
       sum(ws_ext_sales_price) AS itemrevenue,
       sum(ws_ext_sales_price) * 100 / sum(sum(ws_ext_sales_price)) OVER (PARTITION BY i_class) AS revenueratio
  FROM web_sales, item, date_dim
 WHERE ws_item_sk = i_item_sk AND i_category IN (%s) AND ws_sold_date_sk = d_date_sk
   AND d_date BETWEEN date '%s' AND date '%s' + 30
 GROUP BY i_item_id, i_item_desc, i_category, i_class, i_current_price
 ORDER BY i_category, i_class, i_item_id, i_item_desc, revenueratio LIMIT 100]], pick_n(CATEGORIES, 3), d, d))
end

-- Q13: average quantities and prices for combinations of demographics, household and address.
Q[13] = function()
    run(string.format([[
SELECT avg(ss_quantity), avg(ss_ext_sales_price), avg(ss_ext_wholesale_cost), sum(ss_ext_wholesale_cost)
  FROM store_sales, store, customer_demographics, household_demographics, customer_address, date_dim
 WHERE s_store_sk = ss_store_sk AND ss_sold_date_sk = d_date_sk AND d_year = %d
   AND ((ss_hdemo_sk = hd_demo_sk AND cd_demo_sk = ss_cdemo_sk AND cd_marital_status = '%s'
         AND cd_education_status = '%s' AND ss_sales_price BETWEEN 100.00 AND 150.00 AND hd_dep_count = 3)
     OR (ss_hdemo_sk = hd_demo_sk AND cd_demo_sk = ss_cdemo_sk AND cd_marital_status = '%s'
         AND cd_education_status = '%s' AND ss_sales_price BETWEEN 50.00 AND 100.00 AND hd_dep_count = 1)
     OR (ss_hdemo_sk = hd_demo_sk AND cd_demo_sk = ss_cdemo_sk AND cd_marital_status = '%s'
         AND cd_education_status = '%s' AND ss_sales_price BETWEEN 150.00 AND 200.00 AND hd_dep_count = 1))
   AND ((ss_addr_sk = ca_address_sk AND ca_country = 'United States' AND ca_state IN (%s)
         AND ss_net_profit BETWEEN 100 AND 200)
     OR (ss_addr_sk = ca_address_sk AND ca_country = 'United States' AND ca_state IN (%s)
         AND ss_net_profit BETWEEN 150 AND 300)
     OR (ss_addr_sk = ca_address_sk AND ca_country = 'United States' AND ca_state IN (%s)
         AND ss_net_profit BETWEEN 50 AND 250))]],
        year(), pick(MARITAL), pick(EDU), pick(MARITAL), pick(EDU), pick(MARITAL), pick(EDU),
        pick_n(STATES, 3), pick_n(STATES, 3), pick_n(STATES, 3)))
end

-- Q14: items sold through all three channels, by brand, class and category (both parts).
Q[14] = function()
    local y = year(2)
    local cross_items = string.format([[
cross_items AS (
    SELECT i_item_sk ss_item_sk
      FROM item,
           (SELECT iss.i_brand_id brand_id, iss.i_class_id class_id, iss.i_category_id category_id
              FROM store_sales, item iss, date_dim d1
             WHERE ss_item_sk = iss.i_item_sk AND ss_sold_date_sk = d1.d_date_sk AND d1.d_year BETWEEN %d AND %d
            INTERSECT
            SELECT ics.i_brand_id, ics.i_class_id, ics.i_category_id
              FROM catalog_sales, item ics, date_dim d2
             WHERE cs_item_sk = ics.i_item_sk AND cs_sold_date_sk = d2.d_date_sk AND d2.d_year BETWEEN %d AND %d
            INTERSECT
            SELECT iws.i_brand_id, iws.i_class_id, iws.i_category_id
              FROM web_sales, item iws, date_dim d3
             WHERE ws_item_sk = iws.i_item_sk AND ws_sold_date_sk = d3.d_date_sk AND d3.d_year BETWEEN %d AND %d) x
     WHERE i_brand_id = brand_id AND i_class_id = class_id AND i_category_id = category_id),
avg_sales AS (
    SELECT avg(quantity * list_price) average_sales
      FROM (SELECT ss_quantity quantity, ss_list_price list_price FROM store_sales, date_dim
             WHERE ss_sold_date_sk = d_date_sk AND d_year BETWEEN %d AND %d
            UNION ALL
            SELECT cs_quantity, cs_list_price FROM catalog_sales, date_dim
             WHERE cs_sold_date_sk = d_date_sk AND d_year BETWEEN %d AND %d
            UNION ALL
            SELECT ws_quantity, ws_list_price FROM web_sales, date_dim
             WHERE ws_sold_date_sk = d_date_sk AND d_year BETWEEN %d AND %d) x)]],
        y, y + 2, y, y + 2, y, y + 2, y, y + 2, y, y + 2, y, y + 2)
    -- Part a: this year's November sales of those items, above the average, by channel with rollup.
    run("WITH " .. cross_items .. string.format([[

SELECT channel, i_brand_id, i_class_id, i_category_id, sum(sales), sum(number_sales)
  FROM (SELECT 'store' channel, i_brand_id, i_class_id, i_category_id,
               sum(ss_quantity * ss_list_price) sales, count(*) number_sales
          FROM store_sales, item, date_dim
         WHERE ss_item_sk IN (SELECT ss_item_sk FROM cross_items) AND ss_item_sk = i_item_sk
           AND ss_sold_date_sk = d_date_sk AND d_year = %d AND d_moy = 11
         GROUP BY i_brand_id, i_class_id, i_category_id
        HAVING sum(ss_quantity * ss_list_price) > (SELECT average_sales FROM avg_sales)
        UNION ALL
        SELECT 'catalog', i_brand_id, i_class_id, i_category_id,
               sum(cs_quantity * cs_list_price), count(*)
          FROM catalog_sales, item, date_dim
         WHERE cs_item_sk IN (SELECT ss_item_sk FROM cross_items) AND cs_item_sk = i_item_sk
           AND cs_sold_date_sk = d_date_sk AND d_year = %d AND d_moy = 11
         GROUP BY i_brand_id, i_class_id, i_category_id
        HAVING sum(cs_quantity * cs_list_price) > (SELECT average_sales FROM avg_sales)
        UNION ALL
        SELECT 'web', i_brand_id, i_class_id, i_category_id,
               sum(ws_quantity * ws_list_price), count(*)
          FROM web_sales, item, date_dim
         WHERE ws_item_sk IN (SELECT ss_item_sk FROM cross_items) AND ws_item_sk = i_item_sk
           AND ws_sold_date_sk = d_date_sk AND d_year = %d AND d_moy = 11
         GROUP BY i_brand_id, i_class_id, i_category_id
        HAVING sum(ws_quantity * ws_list_price) > (SELECT average_sales FROM avg_sales)) y
 GROUP BY ROLLUP (channel, i_brand_id, i_class_id, i_category_id)
 ORDER BY channel, i_brand_id, i_class_id, i_category_id LIMIT 100]], y + 2, y + 2, y + 2))
    -- Part b: the same week of this year and last year, store channel.
    local ws = week_seq_of(y + 2, 12, rand_int(1, 28))
    run("WITH " .. cross_items .. string.format([[

SELECT this_year.channel ty_channel, this_year.i_brand_id ty_brand, this_year.i_class_id ty_class,
       this_year.i_category_id ty_category, this_year.sales ty_sales, this_year.number_sales ty_number_sales,
       last_year.channel ly_channel, last_year.i_brand_id ly_brand, last_year.i_class_id ly_class,
       last_year.i_category_id ly_category, last_year.sales ly_sales, last_year.number_sales ly_number_sales
  FROM (SELECT 'store' channel, i_brand_id, i_class_id, i_category_id,
               sum(ss_quantity * ss_list_price) sales, count(*) number_sales
          FROM store_sales, item, date_dim
         WHERE ss_item_sk IN (SELECT ss_item_sk FROM cross_items) AND ss_item_sk = i_item_sk
           AND ss_sold_date_sk = d_date_sk AND d_week_seq = %d
         GROUP BY i_brand_id, i_class_id, i_category_id
        HAVING sum(ss_quantity * ss_list_price) > (SELECT average_sales FROM avg_sales)) this_year,
       (SELECT 'store' channel, i_brand_id, i_class_id, i_category_id,
               sum(ss_quantity * ss_list_price) sales, count(*) number_sales
          FROM store_sales, item, date_dim
         WHERE ss_item_sk IN (SELECT ss_item_sk FROM cross_items) AND ss_item_sk = i_item_sk
           AND ss_sold_date_sk = d_date_sk AND d_week_seq = %d
         GROUP BY i_brand_id, i_class_id, i_category_id
        HAVING sum(ss_quantity * ss_list_price) > (SELECT average_sales FROM avg_sales)) last_year
 WHERE this_year.i_brand_id = last_year.i_brand_id AND this_year.i_class_id = last_year.i_class_id
   AND this_year.i_category_id = last_year.i_category_id
 ORDER BY this_year.channel, this_year.i_brand_id, this_year.i_class_id, this_year.i_category_id
 LIMIT 100]], ws, ws - 52))
end

-- Q15: catalog sales by customer zip for a quarter, for selected zip prefixes, states or large sales.
Q[15] = function()
    run(string.format([[
SELECT ca_zip, sum(cs_sales_price)
  FROM catalog_sales, customer, customer_address, date_dim
 WHERE cs_bill_customer_sk = c_customer_sk AND c_current_addr_sk = ca_address_sk
   AND (substr(ca_zip, 1, 5) IN ('85669', '86197', '88274', '83405', '86475', '85392', '85460', '80348', '81792')
        OR ca_state IN ('CA', 'WA', 'GA') OR cs_sales_price > 500)
   AND cs_sold_date_sk = d_date_sk AND d_qoy = %d AND d_year = %d
 GROUP BY ca_zip ORDER BY ca_zip LIMIT 100]], qoy(), year()))
end

-- Q16: catalog orders over 60 days shipped from more than one warehouse and never returned.
Q[16] = function()
    local d = date(60)
    run(string.format([[
SELECT count(DISTINCT cs_order_number) AS order_count, sum(cs_ext_ship_cost) AS total_shipping_cost,
       sum(cs_net_profit) AS total_net_profit
  FROM catalog_sales cs1, date_dim, customer_address, call_center
 WHERE d_date BETWEEN date '%s' AND date '%s' + 60 AND cs1.cs_ship_date_sk = d_date_sk
   AND cs1.cs_ship_addr_sk = ca_address_sk AND ca_state = '%s'
   AND cs1.cs_call_center_sk = cc_call_center_sk AND cc_county IN (%s)
   AND EXISTS (SELECT * FROM catalog_sales cs2
                WHERE cs1.cs_order_number = cs2.cs_order_number AND cs1.cs_warehouse_sk <> cs2.cs_warehouse_sk)
   AND NOT EXISTS (SELECT * FROM catalog_returns cr1 WHERE cs1.cs_order_number = cr1.cr_order_number)
 ORDER BY count(DISTINCT cs_order_number) LIMIT 100]], d, d, pick(STATES), counties(5)))
end

-- Q17: quantities of store sales, their returns and the catalog re-purchases, by item and state.
Q[17] = function()
    local y = year()
    run(string.format([[
SELECT i_item_id, i_item_desc, s_state,
       count(ss_quantity) AS store_sales_quantitycount, avg(ss_quantity) AS store_sales_quantityave,
       stddev_samp(ss_quantity) AS store_sales_quantitystdev,
       stddev_samp(ss_quantity) / avg(ss_quantity) AS store_sales_quantitycov,
       count(sr_return_quantity) AS store_returns_quantitycount, avg(sr_return_quantity) AS store_returns_quantityave,
       stddev_samp(sr_return_quantity) AS store_returns_quantitystdev,
       stddev_samp(sr_return_quantity) / avg(sr_return_quantity) AS store_returns_quantitycov,
       count(cs_quantity) AS catalog_sales_quantitycount, avg(cs_quantity) AS catalog_sales_quantityave,
       stddev_samp(cs_quantity) / avg(cs_quantity) AS catalog_sales_quantitystdev,
       stddev_samp(cs_quantity) / avg(cs_quantity) AS catalog_sales_quantitycov
  FROM store_sales, store_returns, catalog_sales, date_dim d1, date_dim d2, date_dim d3, store, item
 WHERE d1.d_quarter_name = '%dQ1' AND d1.d_date_sk = ss_sold_date_sk
   AND i_item_sk = ss_item_sk AND s_store_sk = ss_store_sk
   AND ss_customer_sk = sr_customer_sk AND ss_item_sk = sr_item_sk AND ss_ticket_number = sr_ticket_number
   AND sr_returned_date_sk = d2.d_date_sk AND d2.d_quarter_name IN ('%dQ1', '%dQ2', '%dQ3')
   AND sr_customer_sk = cs_bill_customer_sk AND sr_item_sk = cs_item_sk
   AND cs_sold_date_sk = d3.d_date_sk AND d3.d_quarter_name IN ('%dQ1', '%dQ2', '%dQ3')
 GROUP BY i_item_id, i_item_desc, s_state
 ORDER BY i_item_id, i_item_desc, s_state LIMIT 100]], y, y, y, y, y, y, y))
end

-- Q18: catalog sales averages by item and customer geography, rollup, for a demographic and birth months.
Q[18] = function()
    run(string.format([[
SELECT i_item_id, ca_country, ca_state, ca_county,
       avg(cast(cs_quantity AS decimal(12,2))) agg1, avg(cast(cs_list_price AS decimal(12,2))) agg2,
       avg(cast(cs_coupon_amt AS decimal(12,2))) agg3, avg(cast(cs_sales_price AS decimal(12,2))) agg4,
       avg(cast(cs_net_profit AS decimal(12,2))) agg5, avg(cast(c_birth_year AS decimal(12,2))) agg6,
       avg(cast(cd1.cd_dep_count AS decimal(12,2))) agg7
  FROM catalog_sales, customer_demographics cd1, customer_demographics cd2, customer, customer_address, date_dim, item
 WHERE cs_sold_date_sk = d_date_sk AND cs_item_sk = i_item_sk AND cs_bill_cdemo_sk = cd1.cd_demo_sk
   AND cs_bill_customer_sk = c_customer_sk AND cd1.cd_gender = '%s' AND cd1.cd_education_status = '%s'
   AND c_current_cdemo_sk = cd2.cd_demo_sk AND c_current_addr_sk = ca_address_sk
   AND c_birth_month IN (%s) AND d_year = %d AND ca_state IN (%s)
 GROUP BY ROLLUP (i_item_id, ca_country, ca_state, ca_county)
 ORDER BY ca_country, ca_state, ca_county, i_item_id LIMIT 100]],
        pick(GENDER), pick(EDU), pick_n({"1","2","3","4","5","6","7","8","9","10","11","12"}, 6):gsub("'", ""),
        year(), pick_n(STATES, 7)))
end

-- Q19: brand revenue from customers living outside the store's zip area, for a manager and month.
Q[19] = function()
    run(string.format([[
SELECT i_brand_id brand_id, i_brand brand, i_manufact_id, i_manufact, sum(ss_ext_sales_price) ext_price
  FROM date_dim, store_sales, item, customer, customer_address, store
 WHERE d_date_sk = ss_sold_date_sk AND ss_item_sk = i_item_sk AND i_manager_id = %d
   AND d_moy = %d AND d_year = %d AND ss_customer_sk = c_customer_sk
   AND c_current_addr_sk = ca_address_sk AND substr(ca_zip, 1, 5) <> substr(s_zip, 1, 5)
   AND ss_store_sk = s_store_sk
 GROUP BY i_brand, i_brand_id, i_manufact_id, i_manufact
 ORDER BY ext_price DESC, i_brand, i_brand_id, i_manufact_id, i_manufact LIMIT 100]],
        rand_int(1, 100), moy(), year()))
end

-- Q20: catalog sales by item over 30 days for three categories, with the share of its class.
Q[20] = function()
    local d = date(30)
    run(string.format([[
SELECT i_item_id, i_item_desc, i_category, i_class, i_current_price,
       sum(cs_ext_sales_price) AS itemrevenue,
       sum(cs_ext_sales_price) * 100 / sum(sum(cs_ext_sales_price)) OVER (PARTITION BY i_class) AS revenueratio
  FROM catalog_sales, item, date_dim
 WHERE cs_item_sk = i_item_sk AND i_category IN (%s) AND cs_sold_date_sk = d_date_sk
   AND d_date BETWEEN date '%s' AND date '%s' + 30
 GROUP BY i_item_id, i_item_desc, i_category, i_class, i_current_price
 ORDER BY i_category, i_class, i_item_id, i_item_desc, revenueratio LIMIT 100]], pick_n(CATEGORIES, 3), d, d))
end

-- Q21: inventory of mid-priced items 30 days before and after a date, by warehouse and item.
Q[21] = function()
    local y, m = rand_int(1998, 2000), rand_int(2, 11)
    local d = string.format("%04d-%02d-%02d", y, m, rand_int(1, 28))
    local p = rand_float(0.99, 1.49)
    run(string.format([[
SELECT *
  FROM (SELECT w_warehouse_name, i_item_id,
               sum(CASE WHEN d_date < date '%s' THEN inv_quantity_on_hand ELSE 0 END) AS inv_before,
               sum(CASE WHEN d_date >= date '%s' THEN inv_quantity_on_hand ELSE 0 END) AS inv_after
          FROM inventory, warehouse, item, date_dim
         WHERE i_current_price BETWEEN %.2f AND %.2f AND i_item_sk = inv_item_sk
           AND inv_warehouse_sk = w_warehouse_sk AND inv_date_sk = d_date_sk
           AND d_date BETWEEN date '%s' - 30 AND date '%s' + 30
         GROUP BY w_warehouse_name, i_item_id) x
 WHERE (CASE WHEN inv_before > 0 THEN inv_after / inv_before ELSE NULL END) BETWEEN 2.0 / 3.0 AND 3.0 / 2.0
 ORDER BY w_warehouse_name, i_item_id LIMIT 100]], d, d, p, p + 0.5, d, d))
end

-- Q22: average inventory on hand over a year, rollup by product, brand, class and category.
Q[22] = function()
    local ms = month_seq(rand_int(1998, 1999), rand_int(1, 12))
    run(string.format([[
SELECT i_product_name, i_brand, i_class, i_category, avg(inv_quantity_on_hand) qoh
  FROM inventory, date_dim, item
 WHERE inv_date_sk = d_date_sk AND inv_item_sk = i_item_sk AND d_month_seq BETWEEN %d AND %d
 GROUP BY ROLLUP (i_product_name, i_brand, i_class, i_category)
 ORDER BY qoh, i_product_name, i_brand, i_class, i_category LIMIT 100]], ms, ms + 11))
end

-- Q23: catalog and web sales of frequently sold items to the best store customers (both parts).
Q[23] = function()
    local y = year(2)
    local m = rand_int(1, 7)
    local ctes = string.format([[
frequent_ss_items AS (
    SELECT substr(i_item_desc, 1, 30) itemdesc, i_item_sk item_sk, d_date solddate, count(*) cnt
      FROM store_sales, date_dim, item
     WHERE ss_sold_date_sk = d_date_sk AND ss_item_sk = i_item_sk AND d_year IN (%d, %d, %d, %d)
     GROUP BY substr(i_item_desc, 1, 30), i_item_sk, d_date HAVING count(*) > 4),
max_store_sales AS (
    SELECT max(csales) tpcds_cmax
      FROM (SELECT c_customer_sk, sum(ss_quantity * ss_sales_price) csales
              FROM store_sales, customer, date_dim
             WHERE ss_customer_sk = c_customer_sk AND ss_sold_date_sk = d_date_sk AND d_year IN (%d, %d, %d, %d)
             GROUP BY c_customer_sk) x),
best_ss_customer AS (
    SELECT c_customer_sk, sum(ss_quantity * ss_sales_price) ssales
      FROM store_sales, customer
     WHERE ss_customer_sk = c_customer_sk
     GROUP BY c_customer_sk
    HAVING sum(ss_quantity * ss_sales_price) > (50 / 100.0) * (SELECT tpcds_cmax FROM max_store_sales))]],
        y, y + 1, y + 2, y + 3, y, y + 1, y + 2, y + 3)
    -- Part a: total catalog and web sales in the month.
    run("WITH " .. ctes .. string.format([[

SELECT sum(sales)
  FROM (SELECT cs_quantity * cs_list_price sales
          FROM catalog_sales, date_dim
         WHERE d_year = %d AND d_moy = %d AND cs_sold_date_sk = d_date_sk
           AND cs_item_sk IN (SELECT item_sk FROM frequent_ss_items)
           AND cs_bill_customer_sk IN (SELECT c_customer_sk FROM best_ss_customer)
        UNION ALL
        SELECT ws_quantity * ws_list_price sales
          FROM web_sales, date_dim
         WHERE d_year = %d AND d_moy = %d AND ws_sold_date_sk = d_date_sk
           AND ws_item_sk IN (SELECT item_sk FROM frequent_ss_items)
           AND ws_bill_customer_sk IN (SELECT c_customer_sk FROM best_ss_customer)) x
 LIMIT 100]], y + 2, m, y + 2, m))
    -- Part b: the same, by customer name.
    run("WITH " .. ctes .. string.format([[

SELECT c_last_name, c_first_name, sales
  FROM (SELECT c_last_name, c_first_name, sum(cs_quantity * cs_list_price) sales
          FROM catalog_sales, customer, date_dim
         WHERE d_year = %d AND d_moy = %d AND cs_sold_date_sk = d_date_sk
           AND cs_item_sk IN (SELECT item_sk FROM frequent_ss_items)
           AND cs_bill_customer_sk IN (SELECT c_customer_sk FROM best_ss_customer)
           AND cs_bill_customer_sk = c_customer_sk
         GROUP BY c_last_name, c_first_name
        UNION ALL
        SELECT c_last_name, c_first_name, sum(ws_quantity * ws_list_price) sales
          FROM web_sales, customer, date_dim
         WHERE d_year = %d AND d_moy = %d AND ws_sold_date_sk = d_date_sk
           AND ws_item_sk IN (SELECT item_sk FROM frequent_ss_items)
           AND ws_bill_customer_sk IN (SELECT c_customer_sk FROM best_ss_customer)
           AND ws_bill_customer_sk = c_customer_sk
         GROUP BY c_last_name, c_first_name) x
 ORDER BY c_last_name, c_first_name, sales LIMIT 100]], y + 2, m, y + 2, m))
end

-- Q24: store sales net of returns by customer for items of a color, for one market (both parts).
Q[24] = function()
    local market = rand_int(1, 10)
    local ssales = string.format([[
ssales AS (
    SELECT c_last_name, c_first_name, s_store_name, ca_state, s_state, i_color, i_current_price,
           i_manager_id, i_units, i_size, sum(ss_net_paid) netpaid
      FROM store_sales, store_returns, store, item, customer, customer_address
     WHERE ss_ticket_number = sr_ticket_number AND ss_item_sk = sr_item_sk AND ss_customer_sk = c_customer_sk
       AND ss_item_sk = i_item_sk AND ss_store_sk = s_store_sk AND c_current_addr_sk = ca_address_sk
       AND c_birth_country <> upper(ca_country) AND s_zip = ca_zip AND s_market_id = %d
     GROUP BY c_last_name, c_first_name, s_store_name, ca_state, s_state, i_color, i_current_price,
              i_manager_id, i_units, i_size)]], market)
    for _ = 1, 2 do
        run("WITH " .. ssales .. string.format([[

SELECT c_last_name, c_first_name, s_store_name, sum(netpaid) paid
  FROM ssales
 WHERE i_color = '%s'
 GROUP BY c_last_name, c_first_name, s_store_name
HAVING sum(netpaid) > (SELECT 0.05 * avg(netpaid) FROM ssales)
 ORDER BY c_last_name, c_first_name, s_store_name]], pick(COLORS)))
    end
end

-- Q25: store sales profit, return losses and catalog profit by item and store, for a quarter and the next two.
Q[25] = function()
    local y = year()
    run(string.format([[
SELECT i_item_id, i_item_desc, s_store_id, s_store_name,
       sum(ss_net_profit) AS store_sales_profit, sum(sr_net_loss) AS store_returns_loss,
       sum(cs_net_profit) AS catalog_sales_profit
  FROM store_sales, store_returns, catalog_sales, date_dim d1, date_dim d2, date_dim d3, store, item
 WHERE d1.d_moy = 4 AND d1.d_year = %d AND d1.d_date_sk = ss_sold_date_sk
   AND i_item_sk = ss_item_sk AND s_store_sk = ss_store_sk
   AND ss_customer_sk = sr_customer_sk AND ss_item_sk = sr_item_sk AND ss_ticket_number = sr_ticket_number
   AND sr_returned_date_sk = d2.d_date_sk AND d2.d_moy BETWEEN 4 AND 10 AND d2.d_year = %d
   AND sr_customer_sk = cs_bill_customer_sk AND sr_item_sk = cs_item_sk
   AND cs_sold_date_sk = d3.d_date_sk AND d3.d_moy BETWEEN 4 AND 10 AND d3.d_year = %d
 GROUP BY i_item_id, i_item_desc, s_store_id, s_store_name
 ORDER BY i_item_id, i_item_desc, s_store_id, s_store_name LIMIT 100]], y, y, y))
end

-- Q26: catalog averages by item for a demographic and promotion.
Q[26] = function()
    run(string.format([[
SELECT i_item_id, avg(cs_quantity) agg1, avg(cs_list_price) agg2, avg(cs_coupon_amt) agg3, avg(cs_sales_price) agg4
  FROM catalog_sales, customer_demographics, date_dim, item, promotion
 WHERE cs_sold_date_sk = d_date_sk AND cs_item_sk = i_item_sk AND cs_bill_cdemo_sk = cd_demo_sk
   AND cs_promo_sk = p_promo_sk AND cd_gender = '%s' AND cd_marital_status = '%s'
   AND cd_education_status = '%s' AND (p_channel_email = 'N' OR p_channel_event = 'N') AND d_year = %d
 GROUP BY i_item_id ORDER BY i_item_id LIMIT 100]], pick(GENDER), pick(MARITAL), pick(EDU), year()))
end

-- Q27: store averages by item and state, rollup, for a demographic.
Q[27] = function()
    run(string.format([[
SELECT i_item_id, s_state, grouping(s_state) g_state,
       avg(ss_quantity) agg1, avg(ss_list_price) agg2, avg(ss_coupon_amt) agg3, avg(ss_sales_price) agg4
  FROM store_sales, customer_demographics, date_dim, store, item
 WHERE ss_sold_date_sk = d_date_sk AND ss_item_sk = i_item_sk AND ss_store_sk = s_store_sk
   AND ss_cdemo_sk = cd_demo_sk AND cd_gender = '%s' AND cd_marital_status = '%s'
   AND cd_education_status = '%s' AND d_year = %d AND s_state IN (%s)
 GROUP BY ROLLUP (i_item_id, s_state)
 ORDER BY i_item_id, s_state LIMIT 100]], pick(GENDER), pick(MARITAL), pick(EDU), year(), pick_n(STATES, 6)))
end

-- Q28: list-price statistics of store sales in six quantity buckets and price, coupon or cost ranges.
Q[28] = function()
    local function b(lo, hi, n)
        local lp, ca, wc = rand_int(0, 190), rand_int(0, 18000), rand_int(0, 80)
        return string.format([[
       (SELECT avg(ss_list_price) b%d_lp, count(ss_list_price) b%d_cnt, count(DISTINCT ss_list_price) b%d_cntd
          FROM store_sales
         WHERE ss_quantity BETWEEN %d AND %d
           AND (ss_list_price BETWEEN %d AND %d OR ss_coupon_amt BETWEEN %d AND %d
                OR ss_wholesale_cost BETWEEN %d AND %d)) b%d]],
            n, n, n, lo, hi, lp, lp + 10, ca, ca + 1000, wc, wc + 20, n)
    end
    run("SELECT * FROM\n" .. b(0, 5, 1) .. ",\n" .. b(6, 10, 2) .. ",\n" .. b(11, 15, 3) .. ",\n"
        .. b(16, 20, 4) .. ",\n" .. b(21, 25, 5) .. ",\n" .. b(26, 30, 6) .. " LIMIT 100")
end

-- Q29: quantities sold in store, returned, and bought again from the catalog, by item and store.
Q[29] = function()
    local y, m = year(2), rand_int(1, 9)
    run(string.format([[
SELECT i_item_id, i_item_desc, s_store_id, s_store_name,
       sum(ss_quantity) AS store_sales_quantity, sum(sr_return_quantity) AS store_returns_quantity,
       sum(cs_quantity) AS catalog_sales_quantity
  FROM store_sales, store_returns, catalog_sales, date_dim d1, date_dim d2, date_dim d3, store, item
 WHERE d1.d_moy = %d AND d1.d_year = %d AND d1.d_date_sk = ss_sold_date_sk
   AND i_item_sk = ss_item_sk AND s_store_sk = ss_store_sk
   AND ss_customer_sk = sr_customer_sk AND ss_item_sk = sr_item_sk AND ss_ticket_number = sr_ticket_number
   AND sr_returned_date_sk = d2.d_date_sk AND d2.d_moy BETWEEN %d AND %d AND d2.d_year = %d
   AND sr_customer_sk = cs_bill_customer_sk AND sr_item_sk = cs_item_sk
   AND cs_sold_date_sk = d3.d_date_sk AND d3.d_year IN (%d, %d, %d)
 GROUP BY i_item_id, i_item_desc, s_store_id, s_store_name
 ORDER BY i_item_id, i_item_desc, s_store_id, s_store_name LIMIT 100]], m, y, m, m + 3, y, y, y + 1, y + 2))
end

-- Q30: customers whose web returns exceed 1.2 times the average of their state.
Q[30] = function()
    run(string.format([[
WITH customer_total_return AS (
    SELECT wr_returning_customer_sk AS ctr_customer_sk, ca_state AS ctr_state, sum(wr_return_amt) AS ctr_total_return
      FROM web_returns, date_dim, customer_address
     WHERE wr_returned_date_sk = d_date_sk AND d_year = %d AND wr_returning_addr_sk = ca_address_sk
     GROUP BY wr_returning_customer_sk, ca_state)
SELECT c_customer_id, c_salutation, c_first_name, c_last_name, c_preferred_cust_flag, c_birth_day,
       c_birth_month, c_birth_year, c_birth_country, c_login, c_email_address, c_last_review_date, ctr_total_return
  FROM customer_total_return ctr1, customer_address, customer,
       (SELECT ctr_state, avg(ctr_total_return) * 1.2 AS state_avg FROM customer_total_return GROUP BY ctr_state) ctr2
 WHERE ctr1.ctr_state = ctr2.ctr_state AND ctr1.ctr_total_return > ctr2.state_avg
   AND ca_address_sk = c_current_addr_sk AND ca_state = '%s' AND ctr1.ctr_customer_sk = c_customer_sk
 ORDER BY c_customer_id, c_salutation, c_first_name, c_last_name, c_preferred_cust_flag, c_birth_day,
          c_birth_month, c_birth_year, c_birth_country, c_login, c_email_address, c_last_review_date,
          ctr_total_return
 LIMIT 100]], year(), pick(STATES)))
end

-- Q31: counties whose web sales grew faster than their store sales over three quarters.
Q[31] = function()
    local y = year()
    run(string.format([[
WITH ss AS (
    SELECT ca_county, d_qoy, d_year, sum(ss_ext_sales_price) AS store_sales
      FROM store_sales, date_dim, customer_address
     WHERE ss_sold_date_sk = d_date_sk AND ss_addr_sk = ca_address_sk
     GROUP BY ca_county, d_qoy, d_year),
ws AS (
    SELECT ca_county, d_qoy, d_year, sum(ws_ext_sales_price) AS web_sales
      FROM web_sales, date_dim, customer_address
     WHERE ws_sold_date_sk = d_date_sk AND ws_bill_addr_sk = ca_address_sk
     GROUP BY ca_county, d_qoy, d_year)
SELECT ss1.ca_county, ss1.d_year,
       ws2.web_sales / ws1.web_sales web_q1_q2_increase, ss2.store_sales / ss1.store_sales store_q1_q2_increase,
       ws3.web_sales / ws2.web_sales web_q2_q3_increase, ss3.store_sales / ss2.store_sales store_q2_q3_increase
  FROM ss ss1, ss ss2, ss ss3, ws ws1, ws ws2, ws ws3
 WHERE ss1.d_qoy = 1 AND ss1.d_year = %d AND ss1.ca_county = ss2.ca_county
   AND ss2.d_qoy = 2 AND ss2.d_year = %d AND ss2.ca_county = ss3.ca_county
   AND ss3.d_qoy = 3 AND ss3.d_year = %d
   AND ss1.ca_county = ws1.ca_county AND ws1.d_qoy = 1 AND ws1.d_year = %d
   AND ws1.ca_county = ws2.ca_county AND ws2.d_qoy = 2 AND ws2.d_year = %d
   AND ws1.ca_county = ws3.ca_county AND ws3.d_qoy = 3 AND ws3.d_year = %d
   AND CASE WHEN ws1.web_sales > 0 THEN ws2.web_sales / ws1.web_sales ELSE NULL END
       > CASE WHEN ss1.store_sales > 0 THEN ss2.store_sales / ss1.store_sales ELSE NULL END
   AND CASE WHEN ws2.web_sales > 0 THEN ws3.web_sales / ws2.web_sales ELSE NULL END
       > CASE WHEN ss2.store_sales > 0 THEN ss3.store_sales / ss2.store_sales ELSE NULL END
 ORDER BY ss1.ca_county]], y, y, y, y, y, y))
end

-- Q32: catalog discounts above 1.3 times the item's average over 90 days, for a manufacturer.
Q[32] = function()
    local d = date(90)
    run(string.format([[
SELECT sum(cs_ext_discount_amt) AS "excess discount amount"
  FROM catalog_sales, item, date_dim
 WHERE i_manufact_id = %d AND i_item_sk = cs_item_sk
   AND d_date BETWEEN date '%s' AND date '%s' + 90 AND d_date_sk = cs_sold_date_sk
   AND cs_ext_discount_amt > (SELECT 1.3 * avg(cs_ext_discount_amt)
                                FROM catalog_sales, date_dim
                               WHERE cs_item_sk = i_item_sk AND d_date BETWEEN date '%s' AND date '%s' + 90
                                 AND d_date_sk = cs_sold_date_sk)
 LIMIT 100]], rand_int(1, 1000), d, d, d, d))
end

-- Q33: sales by manufacturer across the three channels for a category and GMT offset.
Q[33] = function()
    local cat, y, m, g = pick(CATEGORIES), year(), moy(), pick(GMT_OFFSETS)
    local function ch(prefix, sales, cust_addr, date_col, item_col)
        return string.format([[
    SELECT i_manufact_id, sum(%s) total_sales
      FROM %s, date_dim, customer_address, item
     WHERE i_manufact_id IN (SELECT i_manufact_id FROM item WHERE i_category IN ('%s'))
       AND %s = i_item_sk AND %s = d_date_sk AND d_year = %d AND d_moy = %d
       AND %s = ca_address_sk AND ca_gmt_offset = %s
     GROUP BY i_manufact_id]], prefix .. "ext_sales_price", sales, cat, item_col, date_col, y, m, cust_addr, g)
    end
    run("WITH ss AS (" .. ch("ss_", "store_sales", "ss_addr_sk", "ss_sold_date_sk", "ss_item_sk") .. "),\n"
        .. "cs AS (" .. ch("cs_", "catalog_sales", "cs_bill_addr_sk", "cs_sold_date_sk", "cs_item_sk") .. "),\n"
        .. "ws AS (" .. ch("ws_", "web_sales", "ws_bill_addr_sk", "ws_sold_date_sk", "ws_item_sk") .. ")\n"
        .. "SELECT i_manufact_id, sum(total_sales) total_sales\n"
        .. "  FROM (SELECT * FROM ss UNION ALL SELECT * FROM cs UNION ALL SELECT * FROM ws) tmp1\n"
        .. " GROUP BY i_manufact_id ORDER BY total_sales LIMIT 100")
end

-- Q34: customers with store tickets of 15 to 20 items at month start and end, for household profiles.
Q[34] = function()
    local y = year(2)
    run(string.format([[
SELECT c_last_name, c_first_name, c_salutation, c_preferred_cust_flag, ss_ticket_number, cnt
  FROM (SELECT ss_ticket_number, ss_customer_sk, count(*) cnt
          FROM store_sales, date_dim, store, household_demographics
         WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_store_sk = store.s_store_sk
           AND store_sales.ss_hdemo_sk = household_demographics.hd_demo_sk
           AND (date_dim.d_dom BETWEEN 1 AND 3 OR date_dim.d_dom BETWEEN 25 AND 28)
           AND (household_demographics.hd_buy_potential = '%s' OR household_demographics.hd_buy_potential = '%s')
           AND household_demographics.hd_vehicle_count > 0
           AND (CASE WHEN household_demographics.hd_vehicle_count > 0
                     THEN household_demographics.hd_dep_count / household_demographics.hd_vehicle_count ELSE NULL END) > 1.2
           AND date_dim.d_year IN (%d, %d, %d) AND store.s_county IN (%s)
         GROUP BY ss_ticket_number, ss_customer_sk) dn, customer
 WHERE ss_customer_sk = c_customer_sk AND cnt BETWEEN 15 AND 20
 ORDER BY c_last_name, c_first_name, c_salutation, c_preferred_cust_flag DESC, ss_ticket_number]],
        pick(BUY_POTENTIAL), pick(BUY_POTENTIAL), y, y + 1, y + 2, counties(8)))
end

-- Q35: demographics of customers with store sales and web or catalog sales in the first quarters of a year.
Q[35] = function()
    local y = year()
    run(string.format([[
SELECT ca_state, cd_gender, cd_marital_status, cd_dep_count, count(*) cnt1, min(cd_dep_count), max(cd_dep_count), avg(cd_dep_count),
       cd_dep_employed_count, count(*) cnt2, min(cd_dep_employed_count), max(cd_dep_employed_count), avg(cd_dep_employed_count),
       cd_dep_college_count, count(*) cnt3, min(cd_dep_college_count), max(cd_dep_college_count), avg(cd_dep_college_count)
  FROM customer c, customer_address ca, customer_demographics
 WHERE c.c_current_addr_sk = ca.ca_address_sk AND cd_demo_sk = c.c_current_cdemo_sk
   AND EXISTS (SELECT * FROM store_sales, date_dim
                WHERE c.c_customer_sk = ss_customer_sk AND ss_sold_date_sk = d_date_sk AND d_year = %d AND d_qoy < 4)
   AND (EXISTS (SELECT * FROM web_sales, date_dim
                 WHERE c.c_customer_sk = ws_bill_customer_sk AND ws_sold_date_sk = d_date_sk AND d_year = %d AND d_qoy < 4)
        OR EXISTS (SELECT * FROM catalog_sales, date_dim
                    WHERE c.c_customer_sk = cs_ship_customer_sk AND cs_sold_date_sk = d_date_sk AND d_year = %d AND d_qoy < 4))
 GROUP BY ca_state, cd_gender, cd_marital_status, cd_dep_count, cd_dep_employed_count, cd_dep_college_count
 ORDER BY ca_state, cd_gender, cd_marital_status, cd_dep_count, cd_dep_employed_count, cd_dep_college_count
 LIMIT 100]], y, y, y))
end

-- Q36: gross margin by category and class, rollup, ranked within each level, for eight states.
Q[36] = function()
    run(string.format([[
SELECT sum(ss_net_profit) / sum(ss_ext_sales_price) AS gross_margin, i_category, i_class,
       grouping(i_category) + grouping(i_class) AS lochierarchy,
       rank() OVER (PARTITION BY grouping(i_category) + grouping(i_class),
                                 CASE WHEN grouping(i_class) = 0 THEN i_category END
                    ORDER BY sum(ss_net_profit) / sum(ss_ext_sales_price) ASC) AS rank_within_parent
  FROM store_sales, date_dim d1, item, store
 WHERE d1.d_year = %d AND d1.d_date_sk = ss_sold_date_sk AND i_item_sk = ss_item_sk
   AND s_store_sk = ss_store_sk AND s_state IN (%s)
 GROUP BY ROLLUP (i_category, i_class)
 ORDER BY lochierarchy DESC, CASE WHEN grouping(i_category) + grouping(i_class) = 0 THEN i_category END,
          rank_within_parent
 LIMIT 100]], year(), pick_n(STATES, 8)))
end

-- Q37: catalog items in a price range with 100 to 500 units in inventory over 60 days, for four manufacturers.
Q[37] = function()
    local y, m = rand_int(1998, 1999), rand_int(1, 10)
    local d = string.format("%04d-%02d-%02d", y, m, rand_int(1, 28))
    local p = rand_int(1, 60)
    run(string.format([[
SELECT i_item_id, i_item_desc, i_current_price
  FROM item, inventory, date_dim, catalog_sales
 WHERE i_current_price BETWEEN %d AND %d + 30 AND inv_item_sk = i_item_sk AND d_date_sk = inv_date_sk
   AND d_date BETWEEN date '%s' AND date '%s' + 60 AND i_manufact_id IN (%d, %d, %d, %d)
   AND inv_quantity_on_hand BETWEEN 100 AND 500 AND cs_item_sk = i_item_sk
 GROUP BY i_item_id, i_item_desc, i_current_price
 ORDER BY i_item_id LIMIT 100]], p, p, d, d, rand_int(1, 1000), rand_int(1, 1000), rand_int(1, 1000), rand_int(1, 1000)))
end

-- Q38: customers who bought in all three channels over a year, by name and date.
Q[38] = function()
    local ms = month_seq(year(1), moy())
    run(string.format([[
SELECT count(*)
  FROM (SELECT DISTINCT c_last_name, c_first_name, d_date
          FROM store_sales, date_dim, customer
         WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_customer_sk = customer.c_customer_sk
           AND d_month_seq BETWEEN %d AND %d + 11
        INTERSECT
        SELECT DISTINCT c_last_name, c_first_name, d_date
          FROM catalog_sales, date_dim, customer
         WHERE catalog_sales.cs_sold_date_sk = date_dim.d_date_sk AND catalog_sales.cs_bill_customer_sk = customer.c_customer_sk
           AND d_month_seq BETWEEN %d AND %d + 11
        INTERSECT
        SELECT DISTINCT c_last_name, c_first_name, d_date
          FROM web_sales, date_dim, customer
         WHERE web_sales.ws_sold_date_sk = date_dim.d_date_sk AND web_sales.ws_bill_customer_sk = customer.c_customer_sk
           AND d_month_seq BETWEEN %d AND %d + 11) hot_cust
 LIMIT 100]], ms, ms, ms, ms, ms, ms))
end

-- Q39: inventory with a high coefficient of variation in one month and the next (both parts).
Q[39] = function()
    local y, m = rand_int(1998, 1999), rand_int(1, 11)
    for _, threshold in ipairs({1, 1.5}) do
        run(string.format([[
WITH inv AS (
    SELECT w_warehouse_name, w_warehouse_sk, i_item_sk, d_moy, stdev, mean,
           CASE mean WHEN 0 THEN NULL ELSE stdev / mean END cov
      FROM (SELECT w_warehouse_name, w_warehouse_sk, i_item_sk, d_moy,
                   stddev_samp(inv_quantity_on_hand) stdev, avg(inv_quantity_on_hand) mean
              FROM inventory, item, warehouse, date_dim
             WHERE inv_item_sk = i_item_sk AND inv_warehouse_sk = w_warehouse_sk AND inv_date_sk = d_date_sk
               AND d_year = %d
             GROUP BY w_warehouse_name, w_warehouse_sk, i_item_sk, d_moy) foo
     WHERE CASE mean WHEN 0 THEN 0 ELSE stdev / mean END > %s)
SELECT inv1.w_warehouse_sk, inv1.i_item_sk, inv1.d_moy, inv1.mean, inv1.cov,
       inv2.w_warehouse_sk, inv2.i_item_sk, inv2.d_moy, inv2.mean, inv2.cov
  FROM inv inv1, inv inv2
 WHERE inv1.i_item_sk = inv2.i_item_sk AND inv1.w_warehouse_sk = inv2.w_warehouse_sk
   AND inv1.d_moy = %d AND inv2.d_moy = %d
 ORDER BY inv1.w_warehouse_sk, inv1.i_item_sk, inv1.d_moy, inv1.mean, inv1.cov,
          inv2.d_moy, inv2.mean, inv2.cov]], y, tostring(threshold), m, m + 1))
    end
end

-- Q40: catalog sales net of returns 30 days before and after a date, by warehouse state and item.
Q[40] = function()
    local d = date(30)
    local p = rand_int(1, 60)
    run(string.format([[
SELECT w_state, i_item_id,
       sum(CASE WHEN d_date < date '%s' THEN cs_sales_price - coalesce(cr_refunded_cash, 0) ELSE 0 END) AS sales_before,
       sum(CASE WHEN d_date >= date '%s' THEN cs_sales_price - coalesce(cr_refunded_cash, 0) ELSE 0 END) AS sales_after
  FROM catalog_sales LEFT OUTER JOIN catalog_returns
         ON (cs_order_number = cr_order_number AND cs_item_sk = cr_item_sk),
       warehouse, item, date_dim
 WHERE i_current_price BETWEEN %d AND %d + 50 AND i_item_sk = cs_item_sk
   AND cs_warehouse_sk = w_warehouse_sk AND cs_sold_date_sk = d_date_sk
   AND d_date BETWEEN date '%s' - 30 AND date '%s' + 30
 GROUP BY w_state, i_item_id ORDER BY w_state, i_item_id LIMIT 100]], d, d, p, p, d, d))
end

-- Q41: product names of a manufacturer range with many colour, unit and size combinations.
Q[41] = function()
    local m = rand_int(1, 960)
    local function c() return pick(COLORS) end
    run(string.format([[
SELECT DISTINCT i_product_name
  FROM item i1
 WHERE i_manufact_id BETWEEN %d AND %d + 40
   AND (SELECT count(*) AS item_cnt
          FROM item
         WHERE (i_manufact = i1.i_manufact
                AND ((i_category = 'Women' AND (i_color = '%s' OR i_color = '%s') AND (i_units = 'Dozen' OR i_units = 'Each') AND (i_size = 'medium' OR i_size = 'large'))
                  OR (i_category = 'Women' AND (i_color = '%s' OR i_color = '%s') AND (i_units = 'Box' OR i_units = 'Case') AND (i_size = 'petite' OR i_size = 'small'))
                  OR (i_category = 'Men' AND (i_color = '%s' OR i_color = '%s') AND (i_units = 'Pallet' OR i_units = 'Carton') AND (i_size = 'extra large' OR i_size = 'economy'))
                  OR (i_category = 'Men' AND (i_color = '%s' OR i_color = '%s') AND (i_units = 'Ounce' OR i_units = 'Bunch') AND (i_size = 'medium' OR i_size = 'large'))))
            OR (i_manufact = i1.i_manufact
                AND ((i_category = 'Women' AND (i_color = '%s' OR i_color = '%s') AND (i_units = 'Gross' OR i_units = 'Cup') AND (i_size = 'medium' OR i_size = 'large'))
                  OR (i_category = 'Women' AND (i_color = '%s' OR i_color = '%s') AND (i_units = 'Dram' OR i_units = 'Lb') AND (i_size = 'petite' OR i_size = 'small'))
                  OR (i_category = 'Men' AND (i_color = '%s' OR i_color = '%s') AND (i_units = 'Ton' OR i_units = 'Tsp') AND (i_size = 'extra large' OR i_size = 'economy'))
                  OR (i_category = 'Men' AND (i_color = '%s' OR i_color = '%s') AND (i_units = 'Gram' OR i_units = 'Oz') AND (i_size = 'medium' OR i_size = 'large'))))) > 0
 ORDER BY i_product_name LIMIT 100]], m, m, c(), c(), c(), c(), c(), c(), c(), c(), c(), c(), c(), c(), c(), c(), c(), c()))
end

-- Q42: store sales by category for a month and year.
Q[42] = function()
    run(string.format([[
SELECT dt.d_year, item.i_category_id, item.i_category, sum(ss_ext_sales_price)
  FROM date_dim dt, store_sales, item
 WHERE dt.d_date_sk = store_sales.ss_sold_date_sk AND store_sales.ss_item_sk = item.i_item_sk
   AND item.i_manager_id = 1 AND dt.d_moy = %d AND dt.d_year = %d
 GROUP BY dt.d_year, item.i_category_id, item.i_category
 ORDER BY sum(ss_ext_sales_price) DESC, dt.d_year, item.i_category_id, item.i_category LIMIT 100]], moy(), year()))
end

-- Q43: store sales per day of the week by store, for a GMT offset and year.
Q[43] = function()
    run(string.format([[
SELECT s_store_name, s_store_id,
       sum(CASE WHEN d_day_name = 'Sunday'    THEN ss_sales_price END) sun_sales,
       sum(CASE WHEN d_day_name = 'Monday'    THEN ss_sales_price END) mon_sales,
       sum(CASE WHEN d_day_name = 'Tuesday'   THEN ss_sales_price END) tue_sales,
       sum(CASE WHEN d_day_name = 'Wednesday' THEN ss_sales_price END) wed_sales,
       sum(CASE WHEN d_day_name = 'Thursday'  THEN ss_sales_price END) thu_sales,
       sum(CASE WHEN d_day_name = 'Friday'    THEN ss_sales_price END) fri_sales,
       sum(CASE WHEN d_day_name = 'Saturday'  THEN ss_sales_price END) sat_sales
  FROM date_dim, store_sales, store
 WHERE d_date_sk = ss_sold_date_sk AND s_store_sk = ss_store_sk AND s_gmt_offset = %s AND d_year = %d
 GROUP BY s_store_name, s_store_id
 ORDER BY s_store_name, s_store_id, sun_sales, mon_sales, tue_sales, wed_sales, thu_sales, fri_sales, sat_sales
 LIMIT 100]], pick(GMT_OFFSETS), year()))
end

-- Q44: the ten best and ten worst items of a store by average net profit.
Q[44] = function()
    local s = rand_int(1, 12)
    run(string.format([[
SELECT asceding.rnk, i1.i_product_name best_performing, i2.i_product_name worst_performing
  FROM (SELECT * FROM (SELECT item_sk, rank() OVER (ORDER BY rank_col ASC) rnk
                         FROM (SELECT ss_item_sk item_sk, avg(ss_net_profit) rank_col
                                 FROM store_sales ss1 WHERE ss_store_sk = %d
                                GROUP BY ss_item_sk
                               HAVING avg(ss_net_profit) > 0.9 * (SELECT avg(ss_net_profit) rank_col
                                                                    FROM store_sales
                                                                   WHERE ss_store_sk = %d AND ss_addr_sk IS NULL
                                                                   GROUP BY ss_store_sk)) v1) v11
         WHERE rnk < 11) asceding,
       (SELECT * FROM (SELECT item_sk, rank() OVER (ORDER BY rank_col DESC) rnk
                         FROM (SELECT ss_item_sk item_sk, avg(ss_net_profit) rank_col
                                 FROM store_sales ss1 WHERE ss_store_sk = %d
                                GROUP BY ss_item_sk
                               HAVING avg(ss_net_profit) > 0.9 * (SELECT avg(ss_net_profit) rank_col
                                                                    FROM store_sales
                                                                   WHERE ss_store_sk = %d AND ss_addr_sk IS NULL
                                                                   GROUP BY ss_store_sk)) v2) v21
         WHERE rnk < 11) descending,
       item i1, item i2
 WHERE asceding.rnk = descending.rnk AND i1.i_item_sk = asceding.item_sk AND i2.i_item_sk = descending.item_sk
 ORDER BY asceding.rnk LIMIT 100]], s, s, s, s))
end

-- Q45: web sales by customer zip and city for a quarter, for selected zips or items.
Q[45] = function()
    run(string.format([[
SELECT ca_zip, ca_city, sum(ws_sales_price)
  FROM web_sales, customer, customer_address, date_dim, item
 WHERE ws_bill_customer_sk = c_customer_sk AND c_current_addr_sk = ca_address_sk
   AND ws_item_sk = i_item_sk
   AND (substr(ca_zip, 1, 5) IN ('85669', '86197', '88274', '83405', '86475', '85392', '85460', '80348', '81792')
        OR i_item_id IN (SELECT i_item_id FROM item WHERE i_item_sk IN (2, 3, 5, 7, 11, 13, 17, 19, 23, 29)))
   AND ws_sold_date_sk = d_date_sk AND d_qoy = %d AND d_year = %d
 GROUP BY ca_zip, ca_city ORDER BY ca_zip, ca_city LIMIT 100]], qoy(), year()))
end

-- Q46: weekend store tickets of customers from five cities, bought in another city, for a household profile.
Q[46] = function()
    local y = year(2)
    run(string.format([[
SELECT c_last_name, c_first_name, ca_city, bought_city, ss_ticket_number, amt, profit
  FROM (SELECT ss_ticket_number, ss_customer_sk, ca_city bought_city,
               sum(ss_coupon_amt) amt, sum(ss_net_profit) profit
          FROM store_sales, date_dim, store, household_demographics, customer_address
         WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_store_sk = store.s_store_sk
           AND store_sales.ss_hdemo_sk = household_demographics.hd_demo_sk
           AND store_sales.ss_addr_sk = customer_address.ca_address_sk
           AND (household_demographics.hd_dep_count = %d OR household_demographics.hd_vehicle_count = %d)
           AND date_dim.d_dow IN (6, 0) AND date_dim.d_year IN (%d, %d, %d) AND store.s_city IN (%s)
         GROUP BY ss_ticket_number, ss_customer_sk, ss_addr_sk, ca_city) dn, customer, customer_address current_addr
 WHERE ss_customer_sk = c_customer_sk AND customer.c_current_addr_sk = current_addr.ca_address_sk
   AND current_addr.ca_city <> bought_city
 ORDER BY c_last_name, c_first_name, ca_city, bought_city, ss_ticket_number LIMIT 100]],
        rand_int(0, 9), rand_int(0, 5), y, y + 1, y + 2, cities(5)))
end

-- Q47: store sales months that deviate by more than 10% from the yearly average, with neighbours.
Q[47] = function()
    local y = year(1)
    run(string.format([[
WITH v1 AS (
    SELECT i_category, i_brand, s_store_name, s_company_name, d_year, d_moy, sum(ss_sales_price) sum_sales,
           avg(sum(ss_sales_price)) OVER (PARTITION BY i_category, i_brand, s_store_name, s_company_name, d_year) avg_monthly_sales,
           rank() OVER (PARTITION BY i_category, i_brand, s_store_name, s_company_name ORDER BY d_year, d_moy) rn
      FROM item, store_sales, date_dim, store
     WHERE ss_item_sk = i_item_sk AND ss_sold_date_sk = d_date_sk AND ss_store_sk = s_store_sk
       AND (d_year = %d OR (d_year = %d AND d_moy = 12) OR (d_year = %d AND d_moy = 1))
     GROUP BY i_category, i_brand, s_store_name, s_company_name, d_year, d_moy),
v2 AS (
    SELECT v1.i_category, v1.i_brand, v1.s_store_name, v1.s_company_name, v1.d_year, v1.d_moy,
           v1.avg_monthly_sales, v1.sum_sales, v1_lag.sum_sales psum, v1_lead.sum_sales nsum
      FROM v1, v1 v1_lag, v1 v1_lead
     WHERE v1.i_category = v1_lag.i_category AND v1.i_category = v1_lead.i_category
       AND v1.i_brand = v1_lag.i_brand AND v1.i_brand = v1_lead.i_brand
       AND v1.s_store_name = v1_lag.s_store_name AND v1.s_store_name = v1_lead.s_store_name
       AND v1.s_company_name = v1_lag.s_company_name AND v1.s_company_name = v1_lead.s_company_name
       AND v1.rn = v1_lag.rn + 1 AND v1.rn = v1_lead.rn - 1)
SELECT *
  FROM v2
 WHERE d_year = %d AND avg_monthly_sales > 0
   AND CASE WHEN avg_monthly_sales > 0 THEN abs(sum_sales - avg_monthly_sales) / avg_monthly_sales ELSE NULL END > 0.1
 ORDER BY sum_sales - avg_monthly_sales, s_store_name LIMIT 100]], y, y - 1, y + 1, y))
end

-- Q48: store quantity sold for demographic and address combinations in a year.
Q[48] = function()
    run(string.format([[
SELECT sum(ss_quantity)
  FROM store_sales, store, customer_demographics, customer_address, date_dim
 WHERE s_store_sk = ss_store_sk AND ss_sold_date_sk = d_date_sk AND d_year = %d
   AND ((cd_demo_sk = ss_cdemo_sk AND cd_marital_status = '%s' AND cd_education_status = '%s'
         AND ss_sales_price BETWEEN 100.00 AND 150.00)
     OR (cd_demo_sk = ss_cdemo_sk AND cd_marital_status = '%s' AND cd_education_status = '%s'
         AND ss_sales_price BETWEEN 50.00 AND 100.00)
     OR (cd_demo_sk = ss_cdemo_sk AND cd_marital_status = '%s' AND cd_education_status = '%s'
         AND ss_sales_price BETWEEN 150.00 AND 200.00))
   AND ((ss_addr_sk = ca_address_sk AND ca_country = 'United States' AND ca_state IN (%s)
         AND ss_net_profit BETWEEN 0 AND 2000)
     OR (ss_addr_sk = ca_address_sk AND ca_country = 'United States' AND ca_state IN (%s)
         AND ss_net_profit BETWEEN 150 AND 3000)
     OR (ss_addr_sk = ca_address_sk AND ca_country = 'United States' AND ca_state IN (%s)
         AND ss_net_profit BETWEEN 50 AND 25000))]],
        year(), pick(MARITAL), pick(EDU), pick(MARITAL), pick(EDU), pick(MARITAL), pick(EDU),
        pick_n(STATES, 3), pick_n(STATES, 3), pick_n(STATES, 3)))
end

-- Q49: the items with the worst return ratios in each channel, for a month.
Q[49] = function()
    local y, m = year(), moy()
    run(string.format([[
SELECT channel, item, return_ratio, return_rank, currency_rank
  FROM (
    SELECT 'web' AS channel, item, return_ratio, return_rank, currency_rank
      FROM (SELECT item, return_ratio, currency_ratio,
                   rank() OVER (ORDER BY return_ratio) AS return_rank,
                   rank() OVER (ORDER BY currency_ratio) AS currency_rank
              FROM (SELECT ws.ws_item_sk AS item,
                           (cast(sum(coalesce(wr.wr_return_quantity, 0)) AS decimal(15,4)) / cast(sum(coalesce(ws.ws_quantity, 0)) AS decimal(15,4))) AS return_ratio,
                           (cast(sum(coalesce(wr.wr_return_amt, 0)) AS decimal(15,4)) / cast(sum(coalesce(ws.ws_net_paid, 0)) AS decimal(15,4))) AS currency_ratio
                      FROM web_sales ws LEFT OUTER JOIN web_returns wr
                             ON (ws.ws_order_number = wr.wr_order_number AND ws.ws_item_sk = wr.wr_item_sk), date_dim
                     WHERE wr.wr_return_amt > 10000 AND ws.ws_net_profit > 1 AND ws.ws_net_paid > 0 AND ws.ws_quantity > 0
                       AND ws_sold_date_sk = d_date_sk AND d_year = %d AND d_moy = %d
                     GROUP BY ws.ws_item_sk) in_web) web
     WHERE web.return_rank <= 10 OR web.currency_rank <= 10
    UNION
    SELECT 'catalog' AS channel, item, return_ratio, return_rank, currency_rank
      FROM (SELECT item, return_ratio, currency_ratio,
                   rank() OVER (ORDER BY return_ratio) AS return_rank,
                   rank() OVER (ORDER BY currency_ratio) AS currency_rank
              FROM (SELECT cs.cs_item_sk AS item,
                           (cast(sum(coalesce(cr.cr_return_quantity, 0)) AS decimal(15,4)) / cast(sum(coalesce(cs.cs_quantity, 0)) AS decimal(15,4))) AS return_ratio,
                           (cast(sum(coalesce(cr.cr_return_amount, 0)) AS decimal(15,4)) / cast(sum(coalesce(cs.cs_net_paid, 0)) AS decimal(15,4))) AS currency_ratio
                      FROM catalog_sales cs LEFT OUTER JOIN catalog_returns cr
                             ON (cs.cs_order_number = cr.cr_order_number AND cs.cs_item_sk = cr.cr_item_sk), date_dim
                     WHERE cr.cr_return_amount > 10000 AND cs.cs_net_profit > 1 AND cs.cs_net_paid > 0 AND cs.cs_quantity > 0
                       AND cs_sold_date_sk = d_date_sk AND d_year = %d AND d_moy = %d
                     GROUP BY cs.cs_item_sk) in_cat) catalog
     WHERE catalog.return_rank <= 10 OR catalog.currency_rank <= 10
    UNION
    SELECT 'store' AS channel, item, return_ratio, return_rank, currency_rank
      FROM (SELECT item, return_ratio, currency_ratio,
                   rank() OVER (ORDER BY return_ratio) AS return_rank,
                   rank() OVER (ORDER BY currency_ratio) AS currency_rank
              FROM (SELECT sts.ss_item_sk AS item,
                           (cast(sum(coalesce(sr.sr_return_quantity, 0)) AS decimal(15,4)) / cast(sum(coalesce(sts.ss_quantity, 0)) AS decimal(15,4))) AS return_ratio,
                           (cast(sum(coalesce(sr.sr_return_amt, 0)) AS decimal(15,4)) / cast(sum(coalesce(sts.ss_net_paid, 0)) AS decimal(15,4))) AS currency_ratio
                      FROM store_sales sts LEFT OUTER JOIN store_returns sr
                             ON (sts.ss_ticket_number = sr.sr_ticket_number AND sts.ss_item_sk = sr.sr_item_sk), date_dim
                     WHERE sr.sr_return_amt > 10000 AND sts.ss_net_profit > 1 AND sts.ss_net_paid > 0 AND sts.ss_quantity > 0
                       AND ss_sold_date_sk = d_date_sk AND d_year = %d AND d_moy = %d
                     GROUP BY sts.ss_item_sk) in_store) store
     WHERE store.return_rank <= 10 OR store.currency_rank <= 10) x
 ORDER BY channel, return_rank, currency_rank, item LIMIT 100]], y, m, y, m, y, m))
end

-- Q50: store returns by delay since the sale, in 30-day buckets, by store, for a month.
Q[50] = function()
    run(string.format([[
SELECT s_store_name, s_company_id, s_street_number, s_street_name, s_street_type, s_suite_number,
       s_city, s_county, s_state, s_zip,
       sum(CASE WHEN (sr_returned_date_sk - ss_sold_date_sk <= 30) THEN 1 ELSE 0 END) AS "30 days",
       sum(CASE WHEN (sr_returned_date_sk - ss_sold_date_sk > 30) AND (sr_returned_date_sk - ss_sold_date_sk <= 60) THEN 1 ELSE 0 END) AS "31-60 days",
       sum(CASE WHEN (sr_returned_date_sk - ss_sold_date_sk > 60) AND (sr_returned_date_sk - ss_sold_date_sk <= 90) THEN 1 ELSE 0 END) AS "61-90 days",
       sum(CASE WHEN (sr_returned_date_sk - ss_sold_date_sk > 90) AND (sr_returned_date_sk - ss_sold_date_sk <= 120) THEN 1 ELSE 0 END) AS "91-120 days",
       sum(CASE WHEN (sr_returned_date_sk - ss_sold_date_sk > 120) THEN 1 ELSE 0 END) AS ">120 days"
  FROM store_sales, store_returns, store, date_dim d1, date_dim d2
 WHERE d2.d_year = %d AND d2.d_moy = %d AND ss_ticket_number = sr_ticket_number AND ss_item_sk = sr_item_sk
   AND ss_sold_date_sk = d1.d_date_sk AND sr_returned_date_sk = d2.d_date_sk
   AND ss_customer_sk = sr_customer_sk AND ss_store_sk = s_store_sk
 GROUP BY s_store_name, s_company_id, s_street_number, s_street_name, s_street_type, s_suite_number,
          s_city, s_county, s_state, s_zip
 ORDER BY s_store_name, s_company_id, s_street_number, s_street_name, s_street_type, s_suite_number,
          s_city, s_county, s_state, s_zip LIMIT 100]], year(), moy()))
end

-- Q51: items whose cumulative web sales overtake their cumulative store sales over a year.
Q[51] = function()
    local ms = month_seq(year(1), moy())
    run(string.format([[
WITH web_v1 AS (
    SELECT ws_item_sk item_sk, d_date,
           sum(sum(ws_sales_price)) OVER (PARTITION BY ws_item_sk ORDER BY d_date ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) cume_sales
      FROM web_sales, date_dim
     WHERE ws_sold_date_sk = d_date_sk AND d_month_seq BETWEEN %d AND %d + 11 AND ws_item_sk IS NOT NULL
     GROUP BY ws_item_sk, d_date),
store_v1 AS (
    SELECT ss_item_sk item_sk, d_date,
           sum(sum(ss_sales_price)) OVER (PARTITION BY ss_item_sk ORDER BY d_date ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) cume_sales
      FROM store_sales, date_dim
     WHERE ss_sold_date_sk = d_date_sk AND d_month_seq BETWEEN %d AND %d + 11 AND ss_item_sk IS NOT NULL
     GROUP BY ss_item_sk, d_date)
SELECT *
  FROM (SELECT item_sk, d_date, web_sales, store_sales,
               max(web_sales) OVER (PARTITION BY item_sk ORDER BY d_date ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) web_cumulative,
               max(store_sales) OVER (PARTITION BY item_sk ORDER BY d_date ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) store_cumulative
          FROM (SELECT CASE WHEN web.item_sk IS NOT NULL THEN web.item_sk ELSE store.item_sk END item_sk,
                       CASE WHEN web.d_date IS NOT NULL THEN web.d_date ELSE store.d_date END d_date,
                       web.cume_sales web_sales, store.cume_sales store_sales
                  FROM web_v1 web FULL OUTER JOIN store_v1 store
                    ON (web.item_sk = store.item_sk AND web.d_date = store.d_date)) x) y
 WHERE web_cumulative > store_cumulative
 ORDER BY item_sk, d_date LIMIT 100]], ms, ms, ms, ms))
end

-- Q52: store sales by brand for a month and year.
Q[52] = function()
    run(string.format([[
SELECT dt.d_year, item.i_brand_id brand_id, item.i_brand brand, sum(ss_ext_sales_price) ext_price
  FROM date_dim dt, store_sales, item
 WHERE dt.d_date_sk = store_sales.ss_sold_date_sk AND store_sales.ss_item_sk = item.i_item_sk
   AND item.i_manager_id = 1 AND dt.d_moy = %d AND dt.d_year = %d
 GROUP BY dt.d_year, item.i_brand, item.i_brand_id
 ORDER BY dt.d_year, ext_price DESC, brand_id LIMIT 100]], moy(), year()))
end

-- Q53: quarterly store sales of selected category, class and brand combinations against their average.
Q[53] = function()
    local ms = month_seq(year(1), moy())
    run(string.format([[
SELECT *
  FROM (SELECT i_manufact_id, sum(ss_sales_price) sum_sales,
               avg(sum(ss_sales_price)) OVER (PARTITION BY i_manufact_id) avg_quarterly_sales
          FROM item, store_sales, date_dim, store
         WHERE ss_item_sk = i_item_sk AND ss_sold_date_sk = d_date_sk AND ss_store_sk = s_store_sk
           AND d_month_seq IN (%d, %d, %d, %d, %d, %d, %d, %d, %d, %d, %d, %d)
           AND ((i_category IN ('Books', 'Children', 'Electronics')
                 AND i_class IN ('personal', 'portable', 'reference', 'self-help')
                 AND i_brand IN ('scholaramalgamalg #14', 'scholaramalgamalg #7', 'exportiunivamalg #9', 'scholaramalgamalg #9'))
             OR (i_category IN ('Women', 'Music', 'Men')
                 AND i_class IN ('accessories', 'classical', 'fragrances', 'pants')
                 AND i_brand IN ('amalgimporto #1', 'edu packscholar #1', 'exportiimporto #1', 'importoamalg #1')))
         GROUP BY i_manufact_id, d_qoy) tmp1
 WHERE CASE WHEN avg_quarterly_sales > 0 THEN abs(sum_sales - avg_quarterly_sales) / avg_quarterly_sales ELSE NULL END > 0.1
 ORDER BY avg_quarterly_sales, sum_sales, i_manufact_id LIMIT 100]],
        ms, ms + 1, ms + 2, ms + 3, ms + 4, ms + 5, ms + 6, ms + 7, ms + 8, ms + 9, ms + 10, ms + 11))
end

-- Q54: store revenue segments of customers who bought a category and class by web or catalog in a month.
Q[54] = function()
    local y, m = year(1), rand_int(1, 9)
    local cat = rand_int(1, #CATEGORIES)
    run(string.format([[
WITH my_customers AS (
    SELECT DISTINCT c_customer_sk, c_current_addr_sk
      FROM (SELECT cs_sold_date_sk sold_date_sk, cs_bill_customer_sk customer_sk, cs_item_sk item_sk FROM catalog_sales
            UNION ALL
            SELECT ws_sold_date_sk sold_date_sk, ws_bill_customer_sk customer_sk, ws_item_sk item_sk FROM web_sales) cs_or_ws_sales,
           item, date_dim, customer
     WHERE sold_date_sk = d_date_sk AND item_sk = i_item_sk AND i_category = '%s' AND i_class = '%s'
       AND c_customer_sk = cs_or_ws_sales.customer_sk AND d_moy = %d AND d_year = %d),
my_revenue AS (
    SELECT c_customer_sk, sum(ss_ext_sales_price) AS revenue
      FROM my_customers, store_sales, customer_address, store, date_dim
     WHERE c_current_addr_sk = ca_address_sk AND ca_county = s_county AND ca_state = s_state
       AND ss_sold_date_sk = d_date_sk AND c_customer_sk = ss_customer_sk AND s_store_sk = ss_store_sk
       AND d_month_seq BETWEEN (SELECT DISTINCT d_month_seq + 1 FROM date_dim WHERE d_year = %d AND d_moy = %d)
                           AND (SELECT DISTINCT d_month_seq + 3 FROM date_dim WHERE d_year = %d AND d_moy = %d)
     GROUP BY c_customer_sk),
segments AS (
    SELECT cast((revenue / 50) AS int) AS segment FROM my_revenue)
SELECT segment, count(*) AS num_customers, segment * 50 AS segment_base
  FROM segments
 GROUP BY segment ORDER BY segment, num_customers LIMIT 100]],
        CATEGORIES[cat], ({"dresses","pants","infants","athletic","classical","rings","bedding","football","fiction","stereo"})[cat],
        m, y, y, m, y, m))
end

-- Q55: store sales by brand for a manager, month and year.
Q[55] = function()
    run(string.format([[
SELECT i_brand_id brand_id, i_brand brand, sum(ss_ext_sales_price) ext_price
  FROM date_dim, store_sales, item
 WHERE d_date_sk = ss_sold_date_sk AND ss_item_sk = i_item_sk AND i_manager_id = %d
   AND d_moy = %d AND d_year = %d
 GROUP BY i_brand, i_brand_id ORDER BY ext_price DESC, i_brand_id LIMIT 100]], rand_int(1, 100), moy(), year()))
end

-- Q56: sales of items of three colours across the three channels, for a month and GMT offset.
Q[56] = function()
    local colors, y, m, g = pick_n(COLORS, 3), year(), moy(), pick(GMT_OFFSETS)
    local function ch(sales, item, dt, addr, price)
        return string.format([[
    SELECT i_item_id, sum(%s) total_sales
      FROM %s, date_dim, customer_address, item
     WHERE i_item_id IN (SELECT i_item_id FROM item WHERE i_color IN (%s))
       AND %s = i_item_sk AND %s = d_date_sk AND d_year = %d AND d_moy = %d
       AND %s = ca_address_sk AND ca_gmt_offset = %s
     GROUP BY i_item_id]], price, sales, colors, item, dt, y, m, addr, g)
    end
    run("WITH ss AS (" .. ch("store_sales", "ss_item_sk", "ss_sold_date_sk", "ss_addr_sk", "ss_ext_sales_price") .. "),\n"
        .. "cs AS (" .. ch("catalog_sales", "cs_item_sk", "cs_sold_date_sk", "cs_bill_addr_sk", "cs_ext_sales_price") .. "),\n"
        .. "ws AS (" .. ch("web_sales", "ws_item_sk", "ws_sold_date_sk", "ws_bill_addr_sk", "ws_ext_sales_price") .. ")\n"
        .. "SELECT i_item_id, sum(total_sales) total_sales\n"
        .. "  FROM (SELECT * FROM ss UNION ALL SELECT * FROM cs UNION ALL SELECT * FROM ws) tmp1\n"
        .. " GROUP BY i_item_id ORDER BY total_sales, i_item_id LIMIT 100")
end

-- Q57: catalog sales months that deviate from the yearly average by call center, with neighbours.
Q[57] = function()
    local y = year(1)
    run(string.format([[
WITH v1 AS (
    SELECT i_category, i_brand, cc_name, d_year, d_moy, sum(cs_sales_price) sum_sales,
           avg(sum(cs_sales_price)) OVER (PARTITION BY i_category, i_brand, cc_name, d_year) avg_monthly_sales,
           rank() OVER (PARTITION BY i_category, i_brand, cc_name ORDER BY d_year, d_moy) rn
      FROM item, catalog_sales, date_dim, call_center
     WHERE cs_item_sk = i_item_sk AND cs_sold_date_sk = d_date_sk AND cc_call_center_sk = cs_call_center_sk
       AND (d_year = %d OR (d_year = %d AND d_moy = 12) OR (d_year = %d AND d_moy = 1))
     GROUP BY i_category, i_brand, cc_name, d_year, d_moy),
v2 AS (
    SELECT v1.i_category, v1.i_brand, v1.cc_name, v1.d_year, v1.d_moy, v1.avg_monthly_sales, v1.sum_sales,
           v1_lag.sum_sales psum, v1_lead.sum_sales nsum
      FROM v1, v1 v1_lag, v1 v1_lead
     WHERE v1.i_category = v1_lag.i_category AND v1.i_category = v1_lead.i_category
       AND v1.i_brand = v1_lag.i_brand AND v1.i_brand = v1_lead.i_brand
       AND v1.cc_name = v1_lag.cc_name AND v1.cc_name = v1_lead.cc_name
       AND v1.rn = v1_lag.rn + 1 AND v1.rn = v1_lead.rn - 1)
SELECT *
  FROM v2
 WHERE d_year = %d AND avg_monthly_sales > 0
   AND CASE WHEN avg_monthly_sales > 0 THEN abs(sum_sales - avg_monthly_sales) / avg_monthly_sales ELSE NULL END > 0.1
 ORDER BY sum_sales - avg_monthly_sales, cc_name LIMIT 100]], y, y - 1, y + 1, y))
end

-- Q58: items that sold about equally through the three channels in the week of a date.
Q[58] = function()
    local d = date(7)
    local function ch(name, sales, item, dt, price)
        return string.format([[
%s AS (
    SELECT i_item_id item_id, sum(%s) %s_item_rev
      FROM %s, item, date_dim
     WHERE %s = i_item_sk AND d_date IN (SELECT d_date FROM date_dim
                                         WHERE d_week_seq = (SELECT d_week_seq FROM date_dim WHERE d_date = date '%s'))
       AND %s = d_date_sk
     GROUP BY i_item_id)]], name, price, name, sales, item, d, dt)
    end
    run("WITH " .. ch("ss_items", "store_sales", "ss_item_sk", "ss_sold_date_sk", "ss_ext_sales_price") .. ",\n"
        .. ch("cs_items", "catalog_sales", "cs_item_sk", "cs_sold_date_sk", "cs_ext_sales_price") .. ",\n"
        .. ch("ws_items", "web_sales", "ws_item_sk", "ws_sold_date_sk", "ws_ext_sales_price") .. [[

SELECT ss_items.item_id, ss_items_item_rev,
       ss_items_item_rev / ((ss_items_item_rev + cs_items_item_rev + ws_items_item_rev) / 3) * 100 ss_dev,
       cs_items_item_rev, cs_items_item_rev / ((ss_items_item_rev + cs_items_item_rev + ws_items_item_rev) / 3) * 100 cs_dev,
       ws_items_item_rev, ws_items_item_rev / ((ss_items_item_rev + cs_items_item_rev + ws_items_item_rev) / 3) * 100 ws_dev,
       (ss_items_item_rev + cs_items_item_rev + ws_items_item_rev) / 3 average
  FROM ss_items, cs_items, ws_items
 WHERE ss_items.item_id = cs_items.item_id AND ss_items.item_id = ws_items.item_id
   AND ss_items_item_rev BETWEEN 0.9 * cs_items_item_rev AND 1.1 * cs_items_item_rev
   AND ss_items_item_rev BETWEEN 0.9 * ws_items_item_rev AND 1.1 * ws_items_item_rev
   AND cs_items_item_rev BETWEEN 0.9 * ss_items_item_rev AND 1.1 * ss_items_item_rev
   AND cs_items_item_rev BETWEEN 0.9 * ws_items_item_rev AND 1.1 * ws_items_item_rev
   AND ws_items_item_rev BETWEEN 0.9 * ss_items_item_rev AND 1.1 * ss_items_item_rev
   AND ws_items_item_rev BETWEEN 0.9 * cs_items_item_rev AND 1.1 * cs_items_item_rev
 ORDER BY item_id, ss_items_item_rev LIMIT 100]])
end

-- Q59: weekly store sales per day of the week, one year against the next, by store.
Q[59] = function()
    local ms = month_seq(year(1), 1)
    run(string.format([[
WITH wss AS (
    SELECT d_week_seq, ss_store_sk,
           sum(CASE WHEN d_day_name = 'Sunday'    THEN ss_sales_price END) sun_sales,
           sum(CASE WHEN d_day_name = 'Monday'    THEN ss_sales_price END) mon_sales,
           sum(CASE WHEN d_day_name = 'Tuesday'   THEN ss_sales_price END) tue_sales,
           sum(CASE WHEN d_day_name = 'Wednesday' THEN ss_sales_price END) wed_sales,
           sum(CASE WHEN d_day_name = 'Thursday'  THEN ss_sales_price END) thu_sales,
           sum(CASE WHEN d_day_name = 'Friday'    THEN ss_sales_price END) fri_sales,
           sum(CASE WHEN d_day_name = 'Saturday'  THEN ss_sales_price END) sat_sales
      FROM store_sales, date_dim
     WHERE d_date_sk = ss_sold_date_sk
     GROUP BY d_week_seq, ss_store_sk)
SELECT s_store_name1, s_store_id1, d_week_seq1,
       sun_sales1 / sun_sales2, mon_sales1 / mon_sales2, tue_sales1 / tue_sales2, wed_sales1 / wed_sales2,
       thu_sales1 / thu_sales2, fri_sales1 / fri_sales2, sat_sales1 / sat_sales2
  FROM (SELECT s_store_name s_store_name1, wss.d_week_seq d_week_seq1, s_store_id s_store_id1,
               sun_sales sun_sales1, mon_sales mon_sales1, tue_sales tue_sales1, wed_sales wed_sales1,
               thu_sales thu_sales1, fri_sales fri_sales1, sat_sales sat_sales1
          FROM wss, store, date_dim d
         WHERE d.d_week_seq = wss.d_week_seq AND ss_store_sk = s_store_sk AND d_month_seq BETWEEN %d AND %d + 11) y,
       (SELECT s_store_name s_store_name2, wss.d_week_seq d_week_seq2, s_store_id s_store_id2,
               sun_sales sun_sales2, mon_sales mon_sales2, tue_sales tue_sales2, wed_sales wed_sales2,
               thu_sales thu_sales2, fri_sales fri_sales2, sat_sales sat_sales2
          FROM wss, store, date_dim d
         WHERE d.d_week_seq = wss.d_week_seq AND ss_store_sk = s_store_sk AND d_month_seq BETWEEN %d + 12 AND %d + 23) x
 WHERE s_store_id1 = s_store_id2 AND d_week_seq1 = d_week_seq2 - 52
 ORDER BY s_store_name1, s_store_id1, d_week_seq1 LIMIT 100]], ms, ms, ms, ms))
end

-- Q60: sales of a category's items across the three channels, for a month and GMT offset.
Q[60] = function()
    local cat, y, m, g = pick(CATEGORIES), year(), moy(), pick(GMT_OFFSETS)
    local function ch(sales, item, dt, addr, price)
        return string.format([[
    SELECT i_item_id, sum(%s) total_sales
      FROM %s, date_dim, customer_address, item
     WHERE i_item_id IN (SELECT i_item_id FROM item WHERE i_category IN ('%s'))
       AND %s = i_item_sk AND %s = d_date_sk AND d_year = %d AND d_moy = %d
       AND %s = ca_address_sk AND ca_gmt_offset = %s
     GROUP BY i_item_id]], price, sales, cat, item, dt, y, m, addr, g)
    end
    run("WITH ss AS (" .. ch("store_sales", "ss_item_sk", "ss_sold_date_sk", "ss_addr_sk", "ss_ext_sales_price") .. "),\n"
        .. "cs AS (" .. ch("catalog_sales", "cs_item_sk", "cs_sold_date_sk", "cs_bill_addr_sk", "cs_ext_sales_price") .. "),\n"
        .. "ws AS (" .. ch("web_sales", "ws_item_sk", "ws_sold_date_sk", "ws_bill_addr_sk", "ws_ext_sales_price") .. ")\n"
        .. "SELECT i_item_id, sum(total_sales) total_sales\n"
        .. "  FROM (SELECT * FROM ss UNION ALL SELECT * FROM cs UNION ALL SELECT * FROM ws) tmp1\n"
        .. " GROUP BY i_item_id ORDER BY i_item_id, total_sales LIMIT 100")
end

-- Q61: the share of a category's store sales that came with a promotion, for a month and GMT offset.
Q[61] = function()
    local cat, y, m, g = pick(CATEGORIES), year(), moy(), pick(GMT_OFFSETS)
    run(string.format([[
SELECT promotions, total, cast(promotions AS decimal(15,4)) / cast(total AS decimal(15,4)) * 100
  FROM (SELECT sum(ss_ext_sales_price) promotions
          FROM store_sales, store, promotion, date_dim, customer, customer_address, item
         WHERE ss_sold_date_sk = d_date_sk AND ss_store_sk = s_store_sk AND ss_promo_sk = p_promo_sk
           AND ss_customer_sk = c_customer_sk AND ca_address_sk = c_current_addr_sk AND ss_item_sk = i_item_sk
           AND ca_gmt_offset = %s AND i_category = '%s'
           AND (p_channel_dmail = 'Y' OR p_channel_email = 'Y' OR p_channel_tv = 'Y')
           AND s_gmt_offset = %s AND d_year = %d AND d_moy = %d) promotional_sales,
       (SELECT sum(ss_ext_sales_price) total
          FROM store_sales, store, date_dim, customer, customer_address, item
         WHERE ss_sold_date_sk = d_date_sk AND ss_store_sk = s_store_sk AND ss_customer_sk = c_customer_sk
           AND ca_address_sk = c_current_addr_sk AND ss_item_sk = i_item_sk
           AND ca_gmt_offset = %s AND i_category = '%s' AND s_gmt_offset = %s AND d_year = %d AND d_moy = %d) all_sales
 ORDER BY promotions, total LIMIT 100]], g, cat, g, y, m, g, cat, g, y, m))
end

-- Q62: web orders by shipping delay bucket, warehouse, ship mode and site, over a year.
Q[62] = function()
    local ms = month_seq(year(1), moy())
    run(string.format([[
SELECT substr(w_warehouse_name, 1, 20), sm_type, web_name,
       sum(CASE WHEN (ws_ship_date_sk - ws_sold_date_sk <= 30) THEN 1 ELSE 0 END) AS "30 days",
       sum(CASE WHEN (ws_ship_date_sk - ws_sold_date_sk > 30) AND (ws_ship_date_sk - ws_sold_date_sk <= 60) THEN 1 ELSE 0 END) AS "31-60 days",
       sum(CASE WHEN (ws_ship_date_sk - ws_sold_date_sk > 60) AND (ws_ship_date_sk - ws_sold_date_sk <= 90) THEN 1 ELSE 0 END) AS "61-90 days",
       sum(CASE WHEN (ws_ship_date_sk - ws_sold_date_sk > 90) AND (ws_ship_date_sk - ws_sold_date_sk <= 120) THEN 1 ELSE 0 END) AS "91-120 days",
       sum(CASE WHEN (ws_ship_date_sk - ws_sold_date_sk > 120) THEN 1 ELSE 0 END) AS ">120 days"
  FROM web_sales, warehouse, ship_mode, web_site, date_dim
 WHERE d_month_seq BETWEEN %d AND %d + 11 AND ws_ship_date_sk = d_date_sk AND ws_warehouse_sk = w_warehouse_sk
   AND ws_ship_mode_sk = sm_ship_mode_sk AND ws_web_site_sk = web_site_sk
 GROUP BY substr(w_warehouse_name, 1, 20), sm_type, web_name
 ORDER BY substr(w_warehouse_name, 1, 20), sm_type, web_name LIMIT 100]], ms, ms))
end

-- Q63: monthly store sales of selected category, class and brand combinations against their average, by manager.
Q[63] = function()
    local ms = month_seq(year(1), moy())
    run(string.format([[
SELECT *
  FROM (SELECT i_manager_id, sum(ss_sales_price) sum_sales,
               avg(sum(ss_sales_price)) OVER (PARTITION BY i_manager_id) avg_monthly_sales
          FROM item, store_sales, date_dim, store
         WHERE ss_item_sk = i_item_sk AND ss_sold_date_sk = d_date_sk AND ss_store_sk = s_store_sk
           AND d_month_seq IN (%d, %d, %d, %d, %d, %d, %d, %d, %d, %d, %d, %d)
           AND ((i_category IN ('Books', 'Children', 'Electronics')
                 AND i_class IN ('personal', 'portable', 'reference', 'self-help')
                 AND i_brand IN ('scholaramalgamalg #14', 'scholaramalgamalg #7', 'exportiunivamalg #9', 'scholaramalgamalg #9'))
             OR (i_category IN ('Women', 'Music', 'Men')
                 AND i_class IN ('accessories', 'classical', 'fragrances', 'pants')
                 AND i_brand IN ('amalgimporto #1', 'edu packscholar #1', 'exportiimporto #1', 'importoamalg #1')))
         GROUP BY i_manager_id, d_moy) tmp1
 WHERE CASE WHEN avg_monthly_sales > 0 THEN abs(sum_sales - avg_monthly_sales) / avg_monthly_sales ELSE NULL END > 0.1
 ORDER BY i_manager_id, avg_monthly_sales, sum_sales LIMIT 100]],
        ms, ms + 1, ms + 2, ms + 3, ms + 4, ms + 5, ms + 6, ms + 7, ms + 8, ms + 9, ms + 10, ms + 11))
end

-- Q64: store sales of items also sold and returned through the catalog, with the customers' demographics
-- and addresses, one year against the next, for a colour list and price range.
Q[64] = function()
    local y = year(1)
    local p = rand_int(1, 80)
    run(string.format([[
WITH cs_ui AS (
    SELECT cs_item_sk, sum(cs_ext_list_price) AS sale,
           sum(cr_refunded_cash + cr_reversed_charge + cr_store_credit) AS refund
      FROM catalog_sales, catalog_returns
     WHERE cs_item_sk = cr_item_sk AND cs_order_number = cr_order_number
     GROUP BY cs_item_sk
    HAVING sum(cs_ext_list_price) > 2 * sum(cr_refunded_cash + cr_reversed_charge + cr_store_credit)),
cross_sales AS (
    SELECT i_product_name product_name, i_item_sk item_sk, s_store_name store_name, s_zip store_zip,
           ad1.ca_street_number b_street_number, ad1.ca_street_name b_street_name, ad1.ca_city b_city, ad1.ca_zip b_zip,
           ad2.ca_street_number c_street_number, ad2.ca_street_name c_street_name, ad2.ca_city c_city, ad2.ca_zip c_zip,
           d1.d_year AS syear, d2.d_year AS fsyear, d3.d_year s2year, count(*) cnt,
           sum(ss_wholesale_cost) s1, sum(ss_list_price) s2, sum(ss_coupon_amt) s3
      FROM store_sales, store_returns, cs_ui, date_dim d1, date_dim d2, date_dim d3, store, customer,
           customer_demographics cd1, customer_demographics cd2, promotion, household_demographics hd1,
           household_demographics hd2, customer_address ad1, customer_address ad2, income_band ib1, income_band ib2, item
     WHERE ss_store_sk = s_store_sk AND ss_sold_date_sk = d1.d_date_sk AND ss_customer_sk = c_customer_sk
       AND ss_cdemo_sk = cd1.cd_demo_sk AND ss_hdemo_sk = hd1.hd_demo_sk AND ss_addr_sk = ad1.ca_address_sk
       AND ss_item_sk = i_item_sk AND ss_item_sk = sr_item_sk AND ss_ticket_number = sr_ticket_number
       AND ss_item_sk = cs_ui.cs_item_sk AND c_current_cdemo_sk = cd2.cd_demo_sk AND c_current_hdemo_sk = hd2.hd_demo_sk
       AND c_current_addr_sk = ad2.ca_address_sk AND c_first_sales_date_sk = d2.d_date_sk
       AND c_first_shipto_date_sk = d3.d_date_sk AND ss_promo_sk = p_promo_sk
       AND hd1.hd_income_band_sk = ib1.ib_income_band_sk AND hd2.hd_income_band_sk = ib2.ib_income_band_sk
       AND cd1.cd_marital_status <> cd2.cd_marital_status
       AND i_color IN (%s) AND i_current_price BETWEEN %d AND %d + 10 AND i_current_price BETWEEN %d + 1 AND %d + 15
     GROUP BY i_product_name, i_item_sk, s_store_name, s_zip, ad1.ca_street_number, ad1.ca_street_name, ad1.ca_city,
              ad1.ca_zip, ad2.ca_street_number, ad2.ca_street_name, ad2.ca_city, ad2.ca_zip, d1.d_year, d2.d_year, d3.d_year)
SELECT cs1.product_name, cs1.store_name, cs1.store_zip, cs1.b_street_number, cs1.b_street_name, cs1.b_city, cs1.b_zip,
       cs1.c_street_number, cs1.c_street_name, cs1.c_city, cs1.c_zip, cs1.syear, cs1.cnt, cs1.s1 AS s11, cs1.s2 AS s21,
       cs1.s3 AS s31, cs2.s1 AS s12, cs2.s2 AS s22, cs2.s3 AS s32, cs2.syear, cs2.cnt
  FROM cross_sales cs1, cross_sales cs2
 WHERE cs1.item_sk = cs2.item_sk AND cs1.syear = %d AND cs2.syear = %d AND cs2.cnt <= cs1.cnt
   AND cs1.store_name = cs2.store_name AND cs1.store_zip = cs2.store_zip
 ORDER BY cs1.product_name, cs1.store_name, cs2.cnt, cs1.s1, cs2.s1]],
        pick_n(COLORS, 6), p, p, p, p, y, y + 1))
end

-- Q65: store and item pairs whose revenue is at most a tenth of the store's average, over a year.
Q[65] = function()
    local ms = month_seq(year(1), moy())
    run(string.format([[
SELECT s_store_name, i_item_desc, sc.revenue, i_current_price, i_wholesale_cost, i_brand
  FROM store, item,
       (SELECT ss_store_sk, avg(revenue) AS ave
          FROM (SELECT ss_store_sk, ss_item_sk, sum(ss_sales_price) AS revenue
                  FROM store_sales, date_dim
                 WHERE ss_sold_date_sk = d_date_sk AND d_month_seq BETWEEN %d AND %d + 11
                 GROUP BY ss_store_sk, ss_item_sk) sa
         GROUP BY ss_store_sk) sb,
       (SELECT ss_store_sk, ss_item_sk, sum(ss_sales_price) AS revenue
          FROM store_sales, date_dim
         WHERE ss_sold_date_sk = d_date_sk AND d_month_seq BETWEEN %d AND %d + 11
         GROUP BY ss_store_sk, ss_item_sk) sc
 WHERE sb.ss_store_sk = sc.ss_store_sk AND sc.revenue <= 0.1 * sb.ave
   AND s_store_sk = sc.ss_store_sk AND i_item_sk = sc.ss_item_sk
 ORDER BY s_store_name, i_item_desc LIMIT 100]], ms, ms, ms, ms))
end

-- Q66: monthly web and catalog sales and net paid by warehouse for two carriers and a time window.
Q[66] = function()
    local y, t = year(), rand_int(0, 57599)
    local carriers = pick_n({"DHL", "UPS", "FEDEX", "USPS", "ZHOU", "MSC", "TBS", "BOXBUNDLES", "GERMA", "ZOUROS",
        "ALLIANCE", "BARIAN", "ORIENTAL", "UNITED", "HARMON", "GREAT EASTERN", "DIAMOND", "AIRBORNE", "LATVIAN", "PRI"}, 2)
    local function months(prefix, amount)
        local out = {}
        for m = 1, 12 do
            out[#out + 1] = string.format("sum(CASE WHEN d_moy = %d THEN %s * %s_quantity ELSE 0 END) AS %s_m%d",
                m, amount, prefix, (amount:find("sales") and "sales" or "net"), m)
        end
        return table.concat(out, ",\n           ")
    end
    local function ch(prefix, sales, dt, wh, mode)
        return string.format([[
    SELECT w_warehouse_name, w_warehouse_sq_ft, w_city, w_county, w_state, w_country,
           '%s' || ',' || '%s' AS ship_carriers, d_year AS year,
           %s,
           %s
      FROM %s, warehouse, date_dim, time_dim, ship_mode
     WHERE %s = d_date_sk AND %s = w_warehouse_sk AND %s_sold_time_sk = t_time_sk AND %s = sm_ship_mode_sk
       AND d_year = %d AND t_time BETWEEN %d AND %d + 28800 AND sm_carrier IN ('%s', '%s')
     GROUP BY w_warehouse_name, w_warehouse_sq_ft, w_city, w_county, w_state, w_country, d_year]],
            carriers:match("'([^']*)'"), carriers:match("','([^']*)'"),
            months(prefix, prefix .. "_ext_sales_price"), months(prefix, prefix .. "_net_paid"),
            sales, dt, wh, prefix, mode, y, t, t, carriers:match("'([^']*)'"), carriers:match("','([^']*)'"))
    end
    local cols = {}
    for m = 1, 12 do cols[#cols + 1] = string.format("sum(sales_m%d) AS sales_m%d, sum(net_m%d) AS net_m%d", m, m, m, m) end
    run("SELECT w_warehouse_name, w_warehouse_sq_ft, w_city, w_county, w_state, w_country, ship_carriers, year,\n       "
        .. table.concat(cols, ",\n       ") .. "\n  FROM (\n"
        .. ch("ws", "web_sales", "ws_sold_date_sk", "ws_warehouse_sk", "ws_ship_mode_sk") .. "\n    UNION ALL\n"
        .. ch("cs", "catalog_sales", "cs_sold_date_sk", "cs_warehouse_sk", "cs_ship_mode_sk") .. ") x\n"
        .. " GROUP BY w_warehouse_name, w_warehouse_sq_ft, w_city, w_county, w_state, w_country, ship_carriers, year\n"
        .. " ORDER BY w_warehouse_name LIMIT 100")
end

-- Q67: store sales rollup by category, class, brand, product, year, quarter, month and store, top 100 per category.
Q[67] = function()
    local ms = month_seq(year(1), moy())
    run(string.format([[
SELECT *
  FROM (SELECT i_category, i_class, i_brand, i_product_name, d_year, d_qoy, d_moy, s_store_id, sumsales,
               rank() OVER (PARTITION BY i_category ORDER BY sumsales DESC) rk
          FROM (SELECT i_category, i_class, i_brand, i_product_name, d_year, d_qoy, d_moy, s_store_id,
                       sum(coalesce(ss_sales_price * ss_quantity, 0)) sumsales
                  FROM store_sales, date_dim, store, item
                 WHERE ss_sold_date_sk = d_date_sk AND ss_item_sk = i_item_sk AND ss_store_sk = s_store_sk
                   AND d_month_seq BETWEEN %d AND %d + 11
                 GROUP BY ROLLUP (i_category, i_class, i_brand, i_product_name, d_year, d_qoy, d_moy, s_store_id)) dw1) dw2
 WHERE rk <= 100
 ORDER BY i_category, i_class, i_brand, i_product_name, d_year, d_qoy, d_moy, s_store_id, sumsales, rk
 LIMIT 100]], ms, ms))
end

-- Q68: store tickets at the start of the month for two cities, bought in another city, for a household profile.
Q[68] = function()
    local y = year(2)
    run(string.format([[
SELECT c_last_name, c_first_name, ca_city, bought_city, ss_ticket_number, extended_price, extended_tax, list_price
  FROM (SELECT ss_ticket_number, ss_customer_sk, ca_city bought_city, sum(ss_ext_sales_price) extended_price,
               sum(ss_ext_list_price) list_price, sum(ss_ext_tax) extended_tax
          FROM store_sales, date_dim, store, household_demographics, customer_address
         WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_store_sk = store.s_store_sk
           AND store_sales.ss_hdemo_sk = household_demographics.hd_demo_sk
           AND store_sales.ss_addr_sk = customer_address.ca_address_sk
           AND date_dim.d_dom BETWEEN 1 AND 2
           AND (household_demographics.hd_dep_count = %d OR household_demographics.hd_vehicle_count = %d)
           AND date_dim.d_year IN (%d, %d, %d) AND store.s_city IN (%s)
         GROUP BY ss_ticket_number, ss_customer_sk, ss_addr_sk, ca_city) dn, customer, customer_address current_addr
 WHERE ss_customer_sk = c_customer_sk AND customer.c_current_addr_sk = current_addr.ca_address_sk
   AND current_addr.ca_city <> bought_city
 ORDER BY c_last_name, ss_ticket_number LIMIT 100]], rand_int(0, 9), rand_int(0, 5), y, y + 1, y + 2, cities(2)))
end

-- Q69: demographics of customers of three states who bought in store but not by web or catalog in a quarter.
Q[69] = function()
    local y, m = year(), rand_int(1, 10)
    run(string.format([[
SELECT cd_gender, cd_marital_status, cd_education_status, count(*) cnt1, cd_purchase_estimate, count(*) cnt2,
       cd_credit_rating, count(*) cnt3
  FROM customer c, customer_address ca, customer_demographics
 WHERE c.c_current_addr_sk = ca.ca_address_sk AND ca_state IN (%s) AND cd_demo_sk = c.c_current_cdemo_sk
   AND EXISTS (SELECT * FROM store_sales, date_dim
                WHERE c.c_customer_sk = ss_customer_sk AND ss_sold_date_sk = d_date_sk
                  AND d_year = %d AND d_moy BETWEEN %d AND %d + 2)
   AND NOT EXISTS (SELECT * FROM web_sales, date_dim
                    WHERE c.c_customer_sk = ws_bill_customer_sk AND ws_sold_date_sk = d_date_sk
                      AND d_year = %d AND d_moy BETWEEN %d AND %d + 2)
   AND NOT EXISTS (SELECT * FROM catalog_sales, date_dim
                    WHERE c.c_customer_sk = cs_ship_customer_sk AND cs_sold_date_sk = d_date_sk
                      AND d_year = %d AND d_moy BETWEEN %d AND %d + 2)
 GROUP BY cd_gender, cd_marital_status, cd_education_status, cd_purchase_estimate, cd_credit_rating
 ORDER BY cd_gender, cd_marital_status, cd_education_status, cd_purchase_estimate, cd_credit_rating
 LIMIT 100]], pick_n(STATES, 3), y, m, m, y, m, m, y, m, m))
end

-- Q70: store net profit by state and county, rollup and ranked, for the five best states over a year.
Q[70] = function()
    local ms = month_seq(year(1), moy())
    run(string.format([[
SELECT sum(ss_net_profit) AS total_sum, s_state, s_county,
       grouping(s_state) + grouping(s_county) AS lochierarchy,
       rank() OVER (PARTITION BY grouping(s_state) + grouping(s_county),
                                 CASE WHEN grouping(s_county) = 0 THEN s_state END
                    ORDER BY sum(ss_net_profit) DESC) AS rank_within_parent
  FROM store_sales, date_dim d1, store
 WHERE d1.d_month_seq BETWEEN %d AND %d + 11 AND d1.d_date_sk = ss_sold_date_sk AND s_store_sk = ss_store_sk
   AND s_state IN (SELECT s_state
                     FROM (SELECT s_state AS s_state,
                                  rank() OVER (PARTITION BY s_state ORDER BY sum(ss_net_profit) DESC) AS ranking
                             FROM store_sales, store, date_dim
                            WHERE d_month_seq BETWEEN %d AND %d + 11 AND d_date_sk = ss_sold_date_sk
                              AND s_store_sk = ss_store_sk
                            GROUP BY s_state) tmp1
                    WHERE ranking <= 5)
 GROUP BY ROLLUP (s_state, s_county)
 ORDER BY lochierarchy DESC, CASE WHEN grouping(s_state) + grouping(s_county) = 0 THEN s_state END,
          rank_within_parent
 LIMIT 100]], ms, ms, ms, ms))
end

-- Q71: brand sales at breakfast and dinner hours across the three channels, for a manager and month.
Q[71] = function()
    local y, m, mgr = year(), moy(), rand_int(1, 100)
    run(string.format([[
SELECT i_brand_id brand_id, i_brand brand, t_hour, t_minute, sum(ext_price) ext_price
  FROM item,
       (SELECT ws_ext_sales_price AS ext_price, ws_sold_date_sk AS sold_date_sk, ws_item_sk AS sold_item_sk,
               ws_sold_time_sk AS time_sk
          FROM web_sales, date_dim
         WHERE d_date_sk = ws_sold_date_sk AND d_moy = %d AND d_year = %d
        UNION ALL
        SELECT cs_ext_sales_price, cs_sold_date_sk, cs_item_sk, cs_sold_time_sk
          FROM catalog_sales, date_dim
         WHERE d_date_sk = cs_sold_date_sk AND d_moy = %d AND d_year = %d
        UNION ALL
        SELECT ss_ext_sales_price, ss_sold_date_sk, ss_item_sk, ss_sold_time_sk
          FROM store_sales, date_dim
         WHERE d_date_sk = ss_sold_date_sk AND d_moy = %d AND d_year = %d) tmp, time_dim
 WHERE sold_item_sk = i_item_sk AND i_manager_id = %d AND time_sk = t_time_sk
   AND (t_meal_time = 'breakfast' OR t_meal_time = 'dinner')
 GROUP BY i_brand, i_brand_id, t_hour, t_minute
 ORDER BY ext_price DESC, i_brand_id]], m, y, m, y, m, y, mgr))
end

-- Q72: catalog orders that could not be filled from inventory by the ship date, for a household and demographic.
Q[72] = function()
    local y = rand_int(1998, 1999)
    run(string.format([[
SELECT i_item_desc, w_warehouse_name, d1.d_week_seq, sum(CASE WHEN p_promo_sk IS NULL THEN 1 ELSE 0 END) no_promo,
       sum(CASE WHEN p_promo_sk IS NOT NULL THEN 1 ELSE 0 END) promo, count(*) total_cnt
  FROM catalog_sales
  JOIN inventory ON (cs_item_sk = inv_item_sk)
  JOIN warehouse ON (w_warehouse_sk = inv_warehouse_sk)
  JOIN item ON (i_item_sk = cs_item_sk)
  JOIN customer_demographics ON (cs_bill_cdemo_sk = cd_demo_sk)
  JOIN household_demographics ON (cs_bill_hdemo_sk = hd_demo_sk)
  JOIN date_dim d1 ON (cs_sold_date_sk = d1.d_date_sk)
  JOIN date_dim d2 ON (inv_date_sk = d2.d_date_sk)
  JOIN date_dim d3 ON (cs_ship_date_sk = d3.d_date_sk)
  LEFT OUTER JOIN promotion ON (cs_promo_sk = p_promo_sk)
  LEFT OUTER JOIN catalog_returns ON (cr_item_sk = cs_item_sk AND cr_order_number = cs_order_number)
 WHERE d1.d_week_seq = d2.d_week_seq AND inv_quantity_on_hand < cs_quantity AND d3.d_date > d1.d_date + 5
   AND hd_buy_potential = '%s' AND d1.d_year = %d AND cd_marital_status = '%s'
 GROUP BY i_item_desc, w_warehouse_name, d1.d_week_seq
 ORDER BY total_cnt DESC, i_item_desc, w_warehouse_name, d_week_seq LIMIT 100]],
        pick(BUY_POTENTIAL), y, pick(MARITAL)))
end

-- Q73: customers with store tickets of one to five items at the start of the month, for a household profile.
Q[73] = function()
    local y = year(2)
    run(string.format([[
SELECT c_last_name, c_first_name, c_salutation, c_preferred_cust_flag, ss_ticket_number, cnt
  FROM (SELECT ss_ticket_number, ss_customer_sk, count(*) cnt
          FROM store_sales, date_dim, store, household_demographics
         WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_store_sk = store.s_store_sk
           AND store_sales.ss_hdemo_sk = household_demographics.hd_demo_sk
           AND date_dim.d_dom BETWEEN 1 AND 2
           AND (household_demographics.hd_buy_potential = '%s' OR household_demographics.hd_buy_potential = '%s')
           AND household_demographics.hd_vehicle_count > 0
           AND CASE WHEN household_demographics.hd_vehicle_count > 0
                    THEN household_demographics.hd_dep_count / household_demographics.hd_vehicle_count ELSE NULL END > 1
           AND date_dim.d_year IN (%d, %d, %d) AND store.s_county IN (%s)
         GROUP BY ss_ticket_number, ss_customer_sk) dj, customer
 WHERE ss_customer_sk = c_customer_sk AND cnt BETWEEN 1 AND 5
 ORDER BY cnt DESC, c_last_name ASC]], pick(BUY_POTENTIAL), pick(BUY_POTENTIAL), y, y + 1, y + 2, counties(4)))
end

-- Q74: customers whose web net paid grew faster than their store net paid, year over year.
Q[74] = function()
    local y = year(1)
    run(string.format([[
SELECT t_s_secyear.customer_id, t_s_secyear.customer_first_name, t_s_secyear.customer_last_name
  FROM %s,
%s,
%s,
%s
 WHERE t_s_secyear.customer_id = t_s_firstyear.customer_id
   AND t_s_firstyear.customer_id = t_w_secyear.customer_id
   AND t_s_firstyear.customer_id = t_w_firstyear.customer_id
   AND t_s_firstyear.year_total > 0 AND t_w_firstyear.year_total > 0
   AND CASE WHEN t_w_firstyear.year_total > 0 THEN t_w_secyear.year_total / t_w_firstyear.year_total ELSE NULL END
       > CASE WHEN t_s_firstyear.year_total > 0 THEN t_s_secyear.year_total / t_s_firstyear.year_total ELSE NULL END
 ORDER BY t_s_secyear.customer_id, t_s_secyear.customer_first_name, t_s_secyear.customer_last_name
 LIMIT 100]],
        year_total("t_s_firstyear", "s", y, "q74"), year_total("t_s_secyear", "s", y + 1, "q74"),
        year_total("t_w_firstyear", "w", y, "q74"), year_total("t_w_secyear", "w", y + 1, "q74")))
end

-- Q75: items of a category whose sales net of returns fell by more than 10% from one year to the next.
Q[75] = function()
    local y, cat = year(1), pick(CATEGORIES)
    run(string.format([[
WITH all_sales AS (
    SELECT d_year, i_brand_id, i_class_id, i_category_id, i_manufact_id,
           sum(sales_cnt) AS sales_cnt, sum(sales_amt) AS sales_amt
      FROM (SELECT d_year, i_brand_id, i_class_id, i_category_id, i_manufact_id,
                   cs_quantity - coalesce(cr_return_quantity, 0) AS sales_cnt,
                   cs_ext_sales_price - coalesce(cr_return_amount, 0.0) AS sales_amt
              FROM catalog_sales JOIN item ON i_item_sk = cs_item_sk
              JOIN date_dim ON d_date_sk = cs_sold_date_sk
              LEFT JOIN catalog_returns ON (cs_order_number = cr_order_number AND cs_item_sk = cr_item_sk)
             WHERE i_category = '%s'
            UNION
            SELECT d_year, i_brand_id, i_class_id, i_category_id, i_manufact_id,
                   ss_quantity - coalesce(sr_return_quantity, 0) AS sales_cnt,
                   ss_ext_sales_price - coalesce(sr_return_amt, 0.0) AS sales_amt
              FROM store_sales JOIN item ON i_item_sk = ss_item_sk
              JOIN date_dim ON d_date_sk = ss_sold_date_sk
              LEFT JOIN store_returns ON (ss_ticket_number = sr_ticket_number AND ss_item_sk = sr_item_sk)
             WHERE i_category = '%s'
            UNION
            SELECT d_year, i_brand_id, i_class_id, i_category_id, i_manufact_id,
                   ws_quantity - coalesce(wr_return_quantity, 0) AS sales_cnt,
                   ws_ext_sales_price - coalesce(wr_return_amt, 0.0) AS sales_amt
              FROM web_sales JOIN item ON i_item_sk = ws_item_sk
              JOIN date_dim ON d_date_sk = ws_sold_date_sk
              LEFT JOIN web_returns ON (ws_order_number = wr_order_number AND ws_item_sk = wr_item_sk)
             WHERE i_category = '%s') sales_detail
     GROUP BY d_year, i_brand_id, i_class_id, i_category_id, i_manufact_id)
SELECT prev_yr.d_year AS prev_year, curr_yr.d_year AS year, curr_yr.i_brand_id, curr_yr.i_class_id,
       curr_yr.i_category_id, curr_yr.i_manufact_id, prev_yr.sales_cnt AS prev_yr_cnt, curr_yr.sales_cnt AS curr_yr_cnt,
       curr_yr.sales_cnt - prev_yr.sales_cnt AS sales_cnt_diff, curr_yr.sales_amt - prev_yr.sales_amt AS sales_amt_diff
  FROM all_sales curr_yr, all_sales prev_yr
 WHERE curr_yr.i_brand_id = prev_yr.i_brand_id AND curr_yr.i_class_id = prev_yr.i_class_id
   AND curr_yr.i_category_id = prev_yr.i_category_id AND curr_yr.i_manufact_id = prev_yr.i_manufact_id
   AND curr_yr.d_year = %d AND prev_yr.d_year = %d
   AND cast(curr_yr.sales_cnt AS decimal(17,2)) / cast(prev_yr.sales_cnt AS decimal(17,2)) < 0.9
 ORDER BY sales_cnt_diff, sales_amt_diff LIMIT 100]], cat, cat, cat, y + 1, y))
end

-- Q76: sales with a missing dimension key, by channel, year, quarter and category.
Q[76] = function()
    run([[
SELECT channel, col_name, d_year, d_qoy, i_category, count(*) sales_cnt, sum(ext_sales_price) sales_amt
  FROM (SELECT 'store' AS channel, 'ss_store_sk' col_name, d_year, d_qoy, i_category, ss_ext_sales_price ext_sales_price
          FROM store_sales, item, date_dim
         WHERE ss_store_sk IS NULL AND ss_sold_date_sk = d_date_sk AND ss_item_sk = i_item_sk
        UNION ALL
        SELECT 'web' AS channel, 'ws_ship_customer_sk' col_name, d_year, d_qoy, i_category, ws_ext_sales_price ext_sales_price
          FROM web_sales, item, date_dim
         WHERE ws_ship_customer_sk IS NULL AND ws_sold_date_sk = d_date_sk AND ws_item_sk = i_item_sk
        UNION ALL
        SELECT 'catalog' AS channel, 'cs_ship_addr_sk' col_name, d_year, d_qoy, i_category, cs_ext_sales_price ext_sales_price
          FROM catalog_sales, item, date_dim
         WHERE cs_ship_addr_sk IS NULL AND cs_sold_date_sk = d_date_sk AND cs_item_sk = i_item_sk) foo
 GROUP BY channel, col_name, d_year, d_qoy, i_category
 ORDER BY channel, col_name, d_year, d_qoy, i_category LIMIT 100]])
end

-- Q77: profit and returns by channel and store, catalog page or web page over 30 days, with rollup.
Q[77] = function()
    local d = date(30)
    local w = string.format("d_date BETWEEN date '%s' AND date '%s' + 30", d, d)
    run(string.format([[
WITH ss AS (
    SELECT s_store_sk, sum(ss_ext_sales_price) AS sales, sum(ss_net_profit) AS profit
      FROM store_sales, date_dim, store
     WHERE ss_sold_date_sk = d_date_sk AND %s AND ss_store_sk = s_store_sk
     GROUP BY s_store_sk),
sr AS (
    SELECT s_store_sk, sum(sr_return_amt) AS returns, sum(sr_net_loss) AS profit_loss
      FROM store_returns, date_dim, store
     WHERE sr_returned_date_sk = d_date_sk AND %s AND sr_store_sk = s_store_sk
     GROUP BY s_store_sk),
cs AS (
    SELECT cs_call_center_sk, sum(cs_ext_sales_price) AS sales, sum(cs_net_profit) AS profit
      FROM catalog_sales, date_dim
     WHERE cs_sold_date_sk = d_date_sk AND %s
     GROUP BY cs_call_center_sk),
cr AS (
    SELECT cr_call_center_sk, sum(cr_return_amount) AS returns, sum(cr_net_loss) AS profit_loss
      FROM catalog_returns, date_dim
     WHERE cr_returned_date_sk = d_date_sk AND %s
     GROUP BY cr_call_center_sk),
ws AS (
    SELECT wp_web_page_sk, sum(ws_ext_sales_price) AS sales, sum(ws_net_profit) AS profit
      FROM web_sales, date_dim, web_page
     WHERE ws_sold_date_sk = d_date_sk AND %s AND ws_web_page_sk = wp_web_page_sk
     GROUP BY wp_web_page_sk),
wr AS (
    SELECT wp_web_page_sk, sum(wr_return_amt) AS returns, sum(wr_net_loss) AS profit_loss
      FROM web_returns, date_dim, web_page
     WHERE wr_returned_date_sk = d_date_sk AND %s AND wr_web_page_sk = wp_web_page_sk
     GROUP BY wp_web_page_sk)
SELECT channel, id, sum(sales) AS sales, sum(returns) AS returns, sum(profit) AS profit
  FROM (SELECT 'store channel' AS channel, ss.s_store_sk AS id, sales, coalesce(returns, 0) AS returns,
               (profit - coalesce(profit_loss, 0)) AS profit
          FROM ss LEFT JOIN sr ON ss.s_store_sk = sr.s_store_sk
        UNION ALL
        SELECT 'catalog channel' AS channel, cs_call_center_sk AS id, sales, returns, (profit - profit_loss) AS profit
          FROM cs, cr
        UNION ALL
        SELECT 'web channel' AS channel, ws.wp_web_page_sk AS id, sales, coalesce(returns, 0) returns,
               (profit - coalesce(profit_loss, 0)) AS profit
          FROM ws LEFT JOIN wr ON ws.wp_web_page_sk = wr.wp_web_page_sk) x
 GROUP BY ROLLUP (channel, id)
 ORDER BY channel, id LIMIT 100]], w, w, w, w, w, w))
end

-- Q78: items a customer bought in store and never returned, with the web and catalog equivalents, for a year.
Q[78] = function()
    local y = year()
    run(string.format([[
WITH ws AS (
    SELECT d_year AS ws_sold_year, ws_item_sk, ws_bill_customer_sk ws_customer_sk,
           sum(ws_quantity) ws_qty, sum(ws_wholesale_cost) ws_wc, sum(ws_sales_price) ws_sp
      FROM web_sales LEFT JOIN web_returns ON wr_order_number = ws_order_number AND ws_item_sk = wr_item_sk
      JOIN date_dim ON ws_sold_date_sk = d_date_sk
     WHERE wr_order_number IS NULL
     GROUP BY d_year, ws_item_sk, ws_bill_customer_sk),
cs AS (
    SELECT d_year AS cs_sold_year, cs_item_sk, cs_bill_customer_sk cs_customer_sk,
           sum(cs_quantity) cs_qty, sum(cs_wholesale_cost) cs_wc, sum(cs_sales_price) cs_sp
      FROM catalog_sales LEFT JOIN catalog_returns ON cr_order_number = cs_order_number AND cs_item_sk = cr_item_sk
      JOIN date_dim ON cs_sold_date_sk = d_date_sk
     WHERE cr_order_number IS NULL
     GROUP BY d_year, cs_item_sk, cs_bill_customer_sk),
ss AS (
    SELECT d_year AS ss_sold_year, ss_item_sk, ss_customer_sk,
           sum(ss_quantity) ss_qty, sum(ss_wholesale_cost) ss_wc, sum(ss_sales_price) ss_sp
      FROM store_sales LEFT JOIN store_returns ON sr_ticket_number = ss_ticket_number AND ss_item_sk = sr_item_sk
      JOIN date_dim ON ss_sold_date_sk = d_date_sk
     WHERE sr_ticket_number IS NULL
     GROUP BY d_year, ss_item_sk, ss_customer_sk)
SELECT ss_sold_year, ss_item_sk, ss_customer_sk,
       round(ss_qty / (coalesce(ws_qty, 0) + coalesce(cs_qty, 0)), 2) ratio,
       ss_qty store_qty, ss_wc store_wholesale_cost, ss_sp store_sales_price,
       coalesce(ws_qty, 0) + coalesce(cs_qty, 0) other_chan_qty,
       coalesce(ws_wc, 0) + coalesce(cs_wc, 0) other_chan_wholesale_cost,
       coalesce(ws_sp, 0) + coalesce(cs_sp, 0) other_chan_sales_price
  FROM ss LEFT JOIN ws ON (ws_sold_year = ss_sold_year AND ws_item_sk = ss_item_sk AND ws_customer_sk = ss_customer_sk)
          LEFT JOIN cs ON (cs_sold_year = ss_sold_year AND cs_item_sk = ss_item_sk AND cs_customer_sk = ss_customer_sk)
 WHERE (coalesce(ws_qty, 0) > 0 OR coalesce(cs_qty, 0) > 0) AND ss_sold_year = %d
 ORDER BY ss_sold_year, ss_item_sk, ss_customer_sk, ss_qty DESC, ss_wc DESC, ss_sp DESC,
          other_chan_qty, other_chan_wholesale_cost, other_chan_sales_price, ratio
 LIMIT 100]], y))
end

-- Q79: Monday store tickets of mid-sized stores for a household profile, with coupons and profit.
Q[79] = function()
    local y = year(2)
    run(string.format([[
SELECT c_last_name, c_first_name, substr(s_city, 1, 30), ss_ticket_number, amt, profit
  FROM (SELECT ss_ticket_number, ss_customer_sk, store.s_city, sum(ss_coupon_amt) amt, sum(ss_net_profit) profit
          FROM store_sales, date_dim, store, household_demographics
         WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_store_sk = store.s_store_sk
           AND store_sales.ss_hdemo_sk = household_demographics.hd_demo_sk
           AND (household_demographics.hd_dep_count = %d OR household_demographics.hd_vehicle_count > %d)
           AND date_dim.d_dow = 1 AND date_dim.d_year IN (%d, %d, %d)
           AND store.s_number_employees BETWEEN 200 AND 295
         GROUP BY ss_ticket_number, ss_customer_sk, ss_addr_sk, store.s_city) ms, customer
 WHERE ss_customer_sk = c_customer_sk
 ORDER BY c_last_name, c_first_name, substr(s_city, 1, 30), profit LIMIT 100]],
        rand_int(0, 9), rand_int(-1, 4), y, y + 1, y + 2))
end

-- Q80: sales, returns and profit by channel over 30 days for unpromoted items above 50, with rollup.
Q[80] = function()
    local d = date(30)
    local w = string.format("d_date BETWEEN date '%s' AND date '%s' + 30", d, d)
    run(string.format([[
WITH ssr AS (
    SELECT s_store_id AS store_id, sum(ss_ext_sales_price) AS sales, sum(coalesce(sr_return_amt, 0)) AS returns,
           sum(ss_net_profit - coalesce(sr_net_loss, 0)) AS profit
      FROM store_sales LEFT OUTER JOIN store_returns ON (ss_item_sk = sr_item_sk AND ss_ticket_number = sr_ticket_number),
           date_dim, store, item, promotion
     WHERE ss_sold_date_sk = d_date_sk AND %s AND ss_store_sk = s_store_sk AND ss_item_sk = i_item_sk
       AND i_current_price > 50 AND ss_promo_sk = p_promo_sk AND p_channel_tv = 'N'
     GROUP BY s_store_id),
csr AS (
    SELECT cp_catalog_page_id AS catalog_page_id, sum(cs_ext_sales_price) AS sales,
           sum(coalesce(cr_return_amount, 0)) AS returns, sum(cs_net_profit - coalesce(cr_net_loss, 0)) AS profit
      FROM catalog_sales LEFT OUTER JOIN catalog_returns ON (cs_item_sk = cr_item_sk AND cs_order_number = cr_order_number),
           date_dim, catalog_page, item, promotion
     WHERE cs_sold_date_sk = d_date_sk AND %s AND cs_catalog_page_sk = cp_catalog_page_sk AND cs_item_sk = i_item_sk
       AND i_current_price > 50 AND cs_promo_sk = p_promo_sk AND p_channel_tv = 'N'
     GROUP BY cp_catalog_page_id),
wsr AS (
    SELECT web_site_id, sum(ws_ext_sales_price) AS sales, sum(coalesce(wr_return_amt, 0)) AS returns,
           sum(ws_net_profit - coalesce(wr_net_loss, 0)) AS profit
      FROM web_sales LEFT OUTER JOIN web_returns ON (ws_item_sk = wr_item_sk AND ws_order_number = wr_order_number),
           date_dim, web_site, item, promotion
     WHERE ws_sold_date_sk = d_date_sk AND %s AND ws_web_site_sk = web_site_sk AND ws_item_sk = i_item_sk
       AND i_current_price > 50 AND ws_promo_sk = p_promo_sk AND p_channel_tv = 'N'
     GROUP BY web_site_id)
SELECT channel, id, sum(sales) AS sales, sum(returns) AS returns, sum(profit) AS profit
  FROM (SELECT 'store channel' AS channel, 'store' || store_id AS id, sales, returns, profit FROM ssr
        UNION ALL
        SELECT 'catalog channel' AS channel, 'catalog_page' || catalog_page_id AS id, sales, returns, profit FROM csr
        UNION ALL
        SELECT 'web channel' AS channel, 'web_site' || web_site_id AS id, sales, returns, profit FROM wsr) x
 GROUP BY ROLLUP (channel, id)
 ORDER BY channel, id LIMIT 100]], w, w, w))
end

-- Q81: customers whose catalog returns exceed 1.2 times the average of their state.
Q[81] = function()
    run(string.format([[
WITH customer_total_return AS (
    SELECT cr_returning_customer_sk AS ctr_customer_sk, ca_state AS ctr_state, sum(cr_return_amt_inc_tax) AS ctr_total_return
      FROM catalog_returns, date_dim, customer_address
     WHERE cr_returned_date_sk = d_date_sk AND d_year = %d AND cr_returning_addr_sk = ca_address_sk
     GROUP BY cr_returning_customer_sk, ca_state)
SELECT c_customer_id, c_salutation, c_first_name, c_last_name, ca_street_number, ca_street_name, ca_street_type,
       ca_suite_number, ca_city, ca_county, ca_state, ca_zip, ca_country, ca_gmt_offset, ca_location_type, ctr_total_return
  FROM customer_total_return ctr1, customer_address, customer,
       (SELECT ctr_state, avg(ctr_total_return) * 1.2 AS state_avg FROM customer_total_return GROUP BY ctr_state) ctr2
 WHERE ctr1.ctr_state = ctr2.ctr_state AND ctr1.ctr_total_return > ctr2.state_avg
   AND ca_address_sk = c_current_addr_sk AND ca_state = '%s' AND ctr1.ctr_customer_sk = c_customer_sk
 ORDER BY c_customer_id, c_salutation, c_first_name, c_last_name, ca_street_number, ca_street_name, ca_street_type,
          ca_suite_number, ca_city, ca_county, ca_state, ca_zip, ca_country, ca_gmt_offset, ca_location_type, ctr_total_return
 LIMIT 100]], year(), pick(STATES)))
end

-- Q82: store items in a price range with 100 to 500 units in inventory over 60 days, for four manufacturers.
Q[82] = function()
    local y, m = rand_int(1998, 1999), rand_int(1, 10)
    local d = string.format("%04d-%02d-%02d", y, m, rand_int(1, 28))
    local p = rand_int(1, 60)
    run(string.format([[
SELECT i_item_id, i_item_desc, i_current_price
  FROM item, inventory, date_dim, store_sales
 WHERE i_current_price BETWEEN %d AND %d + 30 AND inv_item_sk = i_item_sk AND d_date_sk = inv_date_sk
   AND d_date BETWEEN date '%s' AND date '%s' + 60 AND i_manufact_id IN (%d, %d, %d, %d)
   AND inv_quantity_on_hand BETWEEN 100 AND 500 AND ss_item_sk = i_item_sk
 GROUP BY i_item_id, i_item_desc, i_current_price
 ORDER BY i_item_id LIMIT 100]], p, p, d, d, rand_int(1, 1000), rand_int(1, 1000), rand_int(1, 1000), rand_int(1, 1000)))
end

-- Q83: returned quantities by item across the three channels in the weeks of three dates.
Q[83] = function()
    local d1, d2, d3 = date(7), date(7), date(7)
    local function ch(name, returns, item, dt, qty)
        return string.format([[
%s AS (
    SELECT i_item_id item_id, sum(%s) %s_item_qty
      FROM %s, item, date_dim
     WHERE %s = i_item_sk
       AND d_date IN (SELECT d_date FROM date_dim
                       WHERE d_week_seq IN (SELECT d_week_seq FROM date_dim
                                             WHERE d_date IN (date '%s', date '%s', date '%s')))
       AND %s = d_date_sk
     GROUP BY i_item_id)]], name, qty, name, returns, item, d1, d2, d3, dt)
    end
    run("WITH " .. ch("sr_items", "store_returns", "sr_item_sk", "sr_returned_date_sk", "sr_return_quantity") .. ",\n"
        .. ch("cr_items", "catalog_returns", "cr_item_sk", "cr_returned_date_sk", "cr_return_quantity") .. ",\n"
        .. ch("wr_items", "web_returns", "wr_item_sk", "wr_returned_date_sk", "wr_return_quantity") .. [[

SELECT sr_items.item_id, sr_items_item_qty,
       sr_items_item_qty / (sr_items_item_qty + cr_items_item_qty + wr_items_item_qty) / 3.0 * 100 sr_dev,
       cr_items_item_qty, cr_items_item_qty / (sr_items_item_qty + cr_items_item_qty + wr_items_item_qty) / 3.0 * 100 cr_dev,
       wr_items_item_qty, wr_items_item_qty / (sr_items_item_qty + cr_items_item_qty + wr_items_item_qty) / 3.0 * 100 wr_dev,
       (sr_items_item_qty + cr_items_item_qty + wr_items_item_qty) / 3.0 average
  FROM sr_items, cr_items, wr_items
 WHERE sr_items.item_id = cr_items.item_id AND sr_items.item_id = wr_items.item_id
 ORDER BY sr_items.item_id, sr_items_item_qty LIMIT 100]])
end

-- Q84: customers of a city within an income band, with a store return.
Q[84] = function()
    local lo = rand_int(0, 70000)
    run(string.format([[
SELECT c_customer_id AS customer_id, coalesce(c_last_name, '') || ', ' || coalesce(c_first_name, '') AS customername
  FROM customer, customer_address, customer_demographics, household_demographics, income_band, store_returns
 WHERE ca_city = '%s' AND c_current_addr_sk = ca_address_sk
   AND ib_lower_bound >= %d AND ib_upper_bound <= %d + 50000 AND ib_income_band_sk = hd_income_band_sk
   AND cd_demo_sk = c_current_cdemo_sk AND hd_demo_sk = c_current_hdemo_sk AND sr_cdemo_sk = cd_demo_sk
 ORDER BY c_customer_id LIMIT 100]], pick(sample("city", "SELECT DISTINCT ca_city AS v FROM customer_address")), lo, lo))
end

-- Q85: web return statistics by reason for demographic, price and state combinations, in a year.
Q[85] = function()
    run(string.format([[
SELECT substr(r_reason_desc, 1, 20), avg(ws_quantity), avg(wr_refunded_cash), avg(wr_fee)
  FROM web_sales, web_returns, web_page, customer_demographics cd1, customer_demographics cd2,
       customer_address, date_dim, reason
 WHERE ws_web_page_sk = wp_web_page_sk AND ws_item_sk = wr_item_sk AND ws_order_number = wr_order_number
   AND ws_sold_date_sk = d_date_sk AND d_year = %d AND cd1.cd_demo_sk = wr_refunded_cdemo_sk
   AND cd2.cd_demo_sk = wr_returning_cdemo_sk AND ca_address_sk = wr_refunded_addr_sk AND r_reason_sk = wr_reason_sk
   AND ((cd1.cd_marital_status = '%s' AND cd1.cd_marital_status = cd2.cd_marital_status
         AND cd1.cd_education_status = '%s' AND cd1.cd_education_status = cd2.cd_education_status
         AND ws_sales_price BETWEEN 100.00 AND 150.00)
     OR (cd1.cd_marital_status = '%s' AND cd1.cd_marital_status = cd2.cd_marital_status
         AND cd1.cd_education_status = '%s' AND cd1.cd_education_status = cd2.cd_education_status
         AND ws_sales_price BETWEEN 50.00 AND 100.00)
     OR (cd1.cd_marital_status = '%s' AND cd1.cd_marital_status = cd2.cd_marital_status
         AND cd1.cd_education_status = '%s' AND cd1.cd_education_status = cd2.cd_education_status
         AND ws_sales_price BETWEEN 150.00 AND 200.00))
   AND ((ca_country = 'United States' AND ca_state IN (%s) AND ws_net_profit BETWEEN 100 AND 200)
     OR (ca_country = 'United States' AND ca_state IN (%s) AND ws_net_profit BETWEEN 150 AND 300)
     OR (ca_country = 'United States' AND ca_state IN (%s) AND ws_net_profit BETWEEN 50 AND 250))
 GROUP BY r_reason_desc
 ORDER BY substr(r_reason_desc, 1, 20), avg(ws_quantity), avg(wr_refunded_cash), avg(wr_fee) LIMIT 100]],
        year(), pick(MARITAL), pick(EDU), pick(MARITAL), pick(EDU), pick(MARITAL), pick(EDU),
        pick_n(STATES, 3), pick_n(STATES, 3), pick_n(STATES, 3)))
end

-- Q86: web net paid by category and class, rollup, ranked within each level, over a year.
Q[86] = function()
    local ms = month_seq(year(1), moy())
    run(string.format([[
SELECT sum(ws_net_paid) AS total_sum, i_category, i_class,
       grouping(i_category) + grouping(i_class) AS lochierarchy,
       rank() OVER (PARTITION BY grouping(i_category) + grouping(i_class),
                                 CASE WHEN grouping(i_class) = 0 THEN i_category END
                    ORDER BY sum(ws_net_paid) DESC) AS rank_within_parent
  FROM web_sales, date_dim d1, item
 WHERE d1.d_month_seq BETWEEN %d AND %d + 11 AND d1.d_date_sk = ws_sold_date_sk AND i_item_sk = ws_item_sk
 GROUP BY ROLLUP (i_category, i_class)
 ORDER BY lochierarchy DESC, CASE WHEN grouping(i_category) + grouping(i_class) = 0 THEN i_category END,
          rank_within_parent
 LIMIT 100]], ms, ms))
end

-- Q87: customers and dates with store sales but no catalog and no web sales over a year.
Q[87] = function()
    local ms = month_seq(year(1), moy())
    run(string.format([[
SELECT count(*)
  FROM ((SELECT DISTINCT c_last_name, c_first_name, d_date
           FROM store_sales, date_dim, customer
          WHERE store_sales.ss_sold_date_sk = date_dim.d_date_sk AND store_sales.ss_customer_sk = customer.c_customer_sk
            AND d_month_seq BETWEEN %d AND %d + 11)
        EXCEPT
        (SELECT DISTINCT c_last_name, c_first_name, d_date
           FROM catalog_sales, date_dim, customer
          WHERE catalog_sales.cs_sold_date_sk = date_dim.d_date_sk AND catalog_sales.cs_bill_customer_sk = customer.c_customer_sk
            AND d_month_seq BETWEEN %d AND %d + 11)
        EXCEPT
        (SELECT DISTINCT c_last_name, c_first_name, d_date
           FROM web_sales, date_dim, customer
          WHERE web_sales.ws_sold_date_sk = date_dim.d_date_sk AND web_sales.ws_bill_customer_sk = customer.c_customer_sk
            AND d_month_seq BETWEEN %d AND %d + 11)) cool_cust]], ms, ms, ms, ms, ms, ms))
end

-- Q88: store sales counts in eight half-hour slots of the morning for three household profiles.
Q[88] = function()
    local d1, v1, d2, v2, d3, v3 = rand_int(0, 9), rand_int(0, 5), rand_int(0, 9), rand_int(0, 5), rand_int(0, 9), rand_int(0, 5)
    local function slot(name, h, m_lo, m_hi)
        local mins = m_hi and string.format("t_minute >= %d AND t_minute < %d", m_lo, m_hi) or string.format("t_minute >= %d", m_lo)
        return string.format([[
       (SELECT count(*) %s
          FROM store_sales, household_demographics, time_dim, store
         WHERE ss_sold_time_sk = time_dim.t_time_sk AND ss_hdemo_sk = household_demographics.hd_demo_sk
           AND ss_store_sk = s_store_sk AND time_dim.t_hour = %d AND %s
           AND ((household_demographics.hd_dep_count = %d AND household_demographics.hd_vehicle_count <= %d + 2)
             OR (household_demographics.hd_dep_count = %d AND household_demographics.hd_vehicle_count <= %d + 2)
             OR (household_demographics.hd_dep_count = %d AND household_demographics.hd_vehicle_count <= %d + 2))
           AND store.s_store_name = 'ese') %s]], name, h, mins, d1, v1, d2, v2, d3, v3, name)
    end
    run("SELECT *\n  FROM\n" .. slot("h8_30_to_9", 8, 30) .. ",\n" .. slot("h9_to_9_30", 9, 0, 30) .. ",\n"
        .. slot("h9_30_to_10", 9, 30) .. ",\n" .. slot("h10_to_10_30", 10, 0, 30) .. ",\n"
        .. slot("h10_30_to_11", 10, 30) .. ",\n" .. slot("h11_to_11_30", 11, 0, 30) .. ",\n"
        .. slot("h11_30_to_12", 11, 30) .. ",\n" .. slot("h12_to_12_30", 12, 0, 30))
end

-- Q89: monthly store sales of selected category and class combinations against their average, by store.
Q[89] = function()
    run(string.format([[
SELECT *
  FROM (SELECT i_category, i_class, i_brand, s_store_name, s_company_name, d_moy, sum(ss_sales_price) sum_sales,
               avg(sum(ss_sales_price)) OVER (PARTITION BY i_category, i_brand, s_store_name, s_company_name) avg_monthly_sales
          FROM item, store_sales, date_dim, store
         WHERE ss_item_sk = i_item_sk AND ss_sold_date_sk = d_date_sk AND ss_store_sk = s_store_sk AND d_year = %d
           AND ((i_category IN ('Books', 'Electronics', 'Sports') AND i_class IN ('computers', 'stereo', 'football'))
             OR (i_category IN ('Men', 'Jewelry', 'Women') AND i_class IN ('shirts', 'birdal', 'dresses')))
         GROUP BY i_category, i_class, i_brand, s_store_name, s_company_name, d_moy) tmp1
 WHERE CASE WHEN avg_monthly_sales <> 0 THEN abs(sum_sales - avg_monthly_sales) / avg_monthly_sales ELSE NULL END > 0.1
 ORDER BY sum_sales - avg_monthly_sales, s_store_name LIMIT 100]], year()))
end

-- Q90: ratio of morning to evening web sales on pages of a given size, for a household profile.
Q[90] = function()
    local h1, h2, dep = rand_int(6, 12), rand_int(13, 21), rand_int(0, 9)
    run(string.format([[
SELECT cast(amc AS decimal(15,4)) / nullif(cast(pmc AS decimal(15,4)), 0) am_pm_ratio
  FROM (SELECT count(*) amc
          FROM web_sales, household_demographics, time_dim, web_page
         WHERE ws_sold_time_sk = time_dim.t_time_sk AND ws_ship_hdemo_sk = household_demographics.hd_demo_sk
           AND ws_web_page_sk = web_page.wp_web_page_sk AND time_dim.t_hour BETWEEN %d AND %d + 1
           AND household_demographics.hd_dep_count = %d AND web_page.wp_char_count BETWEEN 5000 AND 5200) at,
       (SELECT count(*) pmc
          FROM web_sales, household_demographics, time_dim, web_page
         WHERE ws_sold_time_sk = time_dim.t_time_sk AND ws_ship_hdemo_sk = household_demographics.hd_demo_sk
           AND ws_web_page_sk = web_page.wp_web_page_sk AND time_dim.t_hour BETWEEN %d AND %d + 1
           AND household_demographics.hd_dep_count = %d AND web_page.wp_char_count BETWEEN 5000 AND 5200) pt
 ORDER BY am_pm_ratio LIMIT 100]], h1, h1, dep, h2, h2, dep))
end

-- Q91: catalog return losses by call center and manager for two demographic profiles, in a month.
Q[91] = function()
    run(string.format([[
SELECT cc_call_center_id Call_Center, cc_name Call_Center_Name, cc_manager Manager, sum(cr_net_loss) Returns_Loss
  FROM call_center, catalog_returns, date_dim, customer, customer_address, customer_demographics, household_demographics
 WHERE cr_call_center_sk = cc_call_center_sk AND cr_returned_date_sk = d_date_sk AND cr_returning_customer_sk = c_customer_sk
   AND cd_demo_sk = c_current_cdemo_sk AND hd_demo_sk = c_current_hdemo_sk AND ca_address_sk = c_current_addr_sk
   AND d_year = %d AND d_moy = %d
   AND ((cd_marital_status = 'M' AND cd_education_status = 'Unknown')
     OR (cd_marital_status = 'W' AND cd_education_status = 'Advanced Degree'))
   AND hd_buy_potential LIKE '%s%%' AND ca_gmt_offset = %s
 GROUP BY cc_call_center_id, cc_name, cc_manager, cd_marital_status, cd_education_status
 ORDER BY sum(cr_net_loss) DESC]], year(), moy(), pick({"Unknown", ">10000", "0-500", "501-1000"}), pick(GMT_OFFSETS)))
end

-- Q92: web discounts above 1.3 times the item's average over 90 days, for a manufacturer.
Q[92] = function()
    local d = date(90)
    run(string.format([[
SELECT sum(ws_ext_discount_amt) AS "Excess Discount Amount"
  FROM web_sales, item, date_dim
 WHERE i_manufact_id = %d AND i_item_sk = ws_item_sk
   AND d_date BETWEEN date '%s' AND date '%s' + 90 AND d_date_sk = ws_sold_date_sk
   AND ws_ext_discount_amt > (SELECT 1.3 * avg(ws_ext_discount_amt)
                                FROM web_sales, date_dim
                               WHERE ws_item_sk = i_item_sk AND d_date BETWEEN date '%s' AND date '%s' + 90
                                 AND d_date_sk = ws_sold_date_sk)
 ORDER BY sum(ws_ext_discount_amt) LIMIT 100]], rand_int(1, 1000), d, d, d, d))
end

-- Q93: store sales net of the returns made for one reason, by customer.
Q[93] = function()
    run(string.format([[
SELECT ss_customer_sk, sum(act_sales) sumsales
  FROM (SELECT ss_item_sk, ss_ticket_number, ss_customer_sk,
               CASE WHEN sr_return_quantity IS NOT NULL THEN (ss_quantity - sr_return_quantity) * ss_sales_price
                    ELSE ss_quantity * ss_sales_price END act_sales
          FROM store_sales LEFT OUTER JOIN store_returns
                 ON (sr_item_sk = ss_item_sk AND sr_ticket_number = ss_ticket_number), reason
         WHERE sr_reason_sk = r_reason_sk AND r_reason_desc = 'reason %d') t
 GROUP BY ss_customer_sk ORDER BY sumsales, ss_customer_sk LIMIT 100]], rand_int(1, 35)))
end

-- Q94: web orders over 60 days shipped from several warehouses and never returned, for a site company.
Q[94] = function()
    local d = date(60)
    run(string.format([[
SELECT count(DISTINCT ws_order_number) AS "order count", sum(ws_ext_ship_cost) AS "total shipping cost",
       sum(ws_net_profit) AS "total net profit"
  FROM web_sales ws1, date_dim, customer_address, web_site
 WHERE d_date BETWEEN date '%s' AND date '%s' + 60 AND ws1.ws_ship_date_sk = d_date_sk
   AND ws1.ws_ship_addr_sk = ca_address_sk AND ca_state = '%s'
   AND ws1.ws_web_site_sk = web_site_sk AND web_company_name = '%s'
   AND EXISTS (SELECT * FROM web_sales ws2
                WHERE ws1.ws_order_number = ws2.ws_order_number AND ws1.ws_warehouse_sk <> ws2.ws_warehouse_sk)
   AND NOT EXISTS (SELECT * FROM web_returns wr1 WHERE ws1.ws_order_number = wr1.wr_order_number)
 ORDER BY count(DISTINCT ws_order_number) LIMIT 100]], d, d, pick(STATES), pick({"ought", "able", "pri", "ese", "anti"})))
end

-- Q95: web orders over 60 days with a second order of the same customer shipped from another warehouse, and returned.
Q[95] = function()
    local d = date(60)
    run(string.format([[
WITH ws_wh AS (
    SELECT ws1.ws_order_number, ws1.ws_warehouse_sk wh1, ws2.ws_warehouse_sk wh2
      FROM web_sales ws1, web_sales ws2
     WHERE ws1.ws_order_number = ws2.ws_order_number AND ws1.ws_warehouse_sk <> ws2.ws_warehouse_sk)
SELECT count(DISTINCT ws_order_number) AS "order count", sum(ws_ext_ship_cost) AS "total shipping cost",
       sum(ws_net_profit) AS "total net profit"
  FROM web_sales ws1, date_dim, customer_address, web_site
 WHERE d_date BETWEEN date '%s' AND date '%s' + 60 AND ws1.ws_ship_date_sk = d_date_sk
   AND ws1.ws_ship_addr_sk = ca_address_sk AND ca_state = '%s'
   AND ws1.ws_web_site_sk = web_site_sk AND web_company_name = '%s'
   AND ws1.ws_order_number IN (SELECT ws_order_number FROM ws_wh)
   AND ws1.ws_order_number IN (SELECT wr_order_number FROM web_returns, ws_wh
                                WHERE wr_order_number = ws_wh.ws_order_number)
 ORDER BY count(DISTINCT ws_order_number) LIMIT 100]], d, d, pick(STATES), pick({"ought", "able", "pri", "ese", "anti"})))
end

-- Q96: store sales at one hour of the evening for a household profile.
Q[96] = function()
    run(string.format([[
SELECT count(*)
  FROM store_sales, household_demographics, time_dim, store
 WHERE ss_sold_time_sk = time_dim.t_time_sk AND ss_hdemo_sk = household_demographics.hd_demo_sk
   AND ss_store_sk = s_store_sk AND time_dim.t_hour = %d AND time_dim.t_minute >= 30
   AND household_demographics.hd_dep_count = %d AND store.s_store_name = 'ese'
 ORDER BY count(*) LIMIT 100]], rand_int(8, 20), rand_int(0, 9)))
end

-- Q97: customers and items bought in store only, by catalog only, or both, over a year.
Q[97] = function()
    local ms = month_seq(year(1), moy())
    run(string.format([[
WITH ssci AS (
    SELECT ss_customer_sk customer_sk, ss_item_sk item_sk
      FROM store_sales, date_dim
     WHERE ss_sold_date_sk = d_date_sk AND d_month_seq BETWEEN %d AND %d + 11
     GROUP BY ss_customer_sk, ss_item_sk),
csci AS (
    SELECT cs_bill_customer_sk customer_sk, cs_item_sk item_sk
      FROM catalog_sales, date_dim
     WHERE cs_sold_date_sk = d_date_sk AND d_month_seq BETWEEN %d AND %d + 11
     GROUP BY cs_bill_customer_sk, cs_item_sk)
SELECT sum(CASE WHEN ssci.customer_sk IS NOT NULL AND csci.customer_sk IS NULL THEN 1 ELSE 0 END) store_only,
       sum(CASE WHEN ssci.customer_sk IS NULL AND csci.customer_sk IS NOT NULL THEN 1 ELSE 0 END) catalog_only,
       sum(CASE WHEN ssci.customer_sk IS NOT NULL AND csci.customer_sk IS NOT NULL THEN 1 ELSE 0 END) store_and_catalog
  FROM ssci FULL OUTER JOIN csci ON (ssci.customer_sk = csci.customer_sk AND ssci.item_sk = csci.item_sk)
 LIMIT 100]], ms, ms, ms, ms))
end

-- Q98: store sales by item over 30 days for three categories, with the share of its class.
Q[98] = function()
    local d = date(30)
    run(string.format([[
SELECT i_item_id, i_item_desc, i_category, i_class, i_current_price,
       sum(ss_ext_sales_price) AS itemrevenue,
       sum(ss_ext_sales_price) * 100 / sum(sum(ss_ext_sales_price)) OVER (PARTITION BY i_class) AS revenueratio
  FROM store_sales, item, date_dim
 WHERE ss_item_sk = i_item_sk AND i_category IN (%s) AND ss_sold_date_sk = d_date_sk
   AND d_date BETWEEN date '%s' AND date '%s' + 30
 GROUP BY i_item_id, i_item_desc, i_category, i_class, i_current_price
 ORDER BY i_category, i_class, i_item_id, i_item_desc, revenueratio]], pick_n(CATEGORIES, 3), d, d))
end

-- Q99: catalog orders by shipping delay bucket, warehouse, ship mode and call center, over a year.
Q[99] = function()
    local ms = month_seq(year(1), moy())
    run(string.format([[
SELECT substr(w_warehouse_name, 1, 20), sm_type, cc_name,
       sum(CASE WHEN (cs_ship_date_sk - cs_sold_date_sk <= 30) THEN 1 ELSE 0 END) AS "30 days",
       sum(CASE WHEN (cs_ship_date_sk - cs_sold_date_sk > 30) AND (cs_ship_date_sk - cs_sold_date_sk <= 60) THEN 1 ELSE 0 END) AS "31-60 days",
       sum(CASE WHEN (cs_ship_date_sk - cs_sold_date_sk > 60) AND (cs_ship_date_sk - cs_sold_date_sk <= 90) THEN 1 ELSE 0 END) AS "61-90 days",
       sum(CASE WHEN (cs_ship_date_sk - cs_sold_date_sk > 90) AND (cs_ship_date_sk - cs_sold_date_sk <= 120) THEN 1 ELSE 0 END) AS "91-120 days",
       sum(CASE WHEN (cs_ship_date_sk - cs_sold_date_sk > 120) THEN 1 ELSE 0 END) AS ">120 days"
  FROM catalog_sales, warehouse, ship_mode, call_center, date_dim
 WHERE d_month_seq BETWEEN %d AND %d + 11 AND cs_ship_date_sk = d_date_sk AND cs_warehouse_sk = w_warehouse_sk
   AND cs_ship_mode_sk = sm_ship_mode_sk AND cs_call_center_sk = cc_call_center_sk
 GROUP BY substr(w_warehouse_name, 1, 20), sm_type, cc_name
 ORDER BY substr(w_warehouse_name, 1, 20), sm_type, cc_name LIMIT 100]], ms, ms))
end

-- Q is global; transactions dispatch with the inline script "Q[n]()".
