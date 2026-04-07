# Statistical analysis module for sales performance data.
# Provides summary statistics, trend detection, and anomaly flagging.

library(dplyr)
library(lubridate)
library(ggplot2)

#' SalesRecord structure:
#'   date       - Date of the sale
#'   region     - Geographic region
#'   product    - Product category
#'   revenue    - Sale amount in USD
#'   units_sold - Number of units

#' Computes summary statistics for sales data grouped by region.
#' Returns a data frame with mean, median, standard deviation,
#' and total revenue per region.
#'
#' @param sales_data A data frame with columns: date, region, revenue, units_sold
#' @return A data frame with per-region statistics
compute_regional_summary <- function(sales_data) {
  if (!all(c("region", "revenue", "units_sold") %in% names(sales_data))) {
    stop("sales_data must contain 'region', 'revenue', and 'units_sold' columns")
  }

  summary <- sales_data %>%
    group_by(region) %>%
    summarise(
      total_revenue   = sum(revenue, na.rm = TRUE),
      mean_revenue    = mean(revenue, na.rm = TRUE),
      median_revenue  = median(revenue, na.rm = TRUE),
      sd_revenue      = sd(revenue, na.rm = TRUE),
      total_units     = sum(units_sold, na.rm = TRUE),
      transaction_count = n(),
      .groups = "drop"
    ) %>%
    arrange(desc(total_revenue))

  return(summary)
}

#' Detects monthly revenue trends using linear regression.
#' Fits a simple linear model of monthly revenue over time
#' and returns the slope, p-value, and trend direction.
#'
#' @param sales_data A data frame with columns: date, revenue
#' @return A list with slope, p_value, direction, and monthly_data
detect_revenue_trend <- function(sales_data) {
  if (!("date" %in% names(sales_data))) {
    stop("sales_data must contain a 'date' column")
  }

  monthly <- sales_data %>%
    mutate(month = floor_date(date, "month")) %>%
    group_by(month) %>%
    summarise(
      monthly_revenue = sum(revenue, na.rm = TRUE),
      .groups = "drop"
    ) %>%
    arrange(month) %>%
    mutate(month_index = row_number())

  if (nrow(monthly) < 3) {
    return(list(
      slope = NA,
      p_value = NA,
      direction = "insufficient_data",
      monthly_data = monthly
    ))
  }

  model <- lm(monthly_revenue ~ month_index, data = monthly)
  slope <- coef(model)[["month_index"]]
  p_value <- summary(model)$coefficients["month_index", "Pr(>|t|)"]

  direction <- if (p_value < 0.05 && slope > 0) {
    "increasing"
  } else if (p_value < 0.05 && slope < 0) {
    "decreasing"
  } else {
    "stable"
  }

  return(list(
    slope = slope,
    p_value = p_value,
    direction = direction,
    monthly_data = monthly
  ))
}

#' Identifies anomalous sales days where revenue deviates more than
#' the specified number of standard deviations from the rolling mean.
#' Uses a 30-day rolling window for baseline calculation.
#'
#' @param sales_data A data frame with columns: date, revenue
#' @param threshold Number of standard deviations for anomaly cutoff (default 2)
#' @return A data frame of anomalous dates with their deviation scores
flag_anomalies <- function(sales_data, threshold = 2) {
  daily <- sales_data %>%
    group_by(date) %>%
    summarise(daily_revenue = sum(revenue, na.rm = TRUE), .groups = "drop") %>%
    arrange(date)

  window_size <- 30

  daily <- daily %>%
    mutate(
      rolling_mean = zoo::rollmean(daily_revenue, k = window_size, fill = NA, align = "right"),
      rolling_sd   = zoo::rollapply(daily_revenue, width = window_size,
                                     FUN = sd, fill = NA, align = "right")
    ) %>%
    filter(!is.na(rolling_mean)) %>%
    mutate(
      z_score = (daily_revenue - rolling_mean) / rolling_sd,
      is_anomaly = abs(z_score) > threshold
    )

  anomalies <- daily %>%
    filter(is_anomaly) %>%
    select(date, daily_revenue, rolling_mean, z_score) %>%
    arrange(desc(abs(z_score)))

  return(anomalies)
}

#' Generates a revenue trend plot with anomaly markers.
#'
#' @param sales_data A data frame with columns: date, revenue
#' @param anomalies A data frame from flag_anomalies()
#' @return A ggplot object
plot_revenue_with_anomalies <- function(sales_data, anomalies) {
  daily <- sales_data %>%
    group_by(date) %>%
    summarise(daily_revenue = sum(revenue, na.rm = TRUE), .groups = "drop")

  p <- ggplot(daily, aes(x = date, y = daily_revenue)) +
    geom_line(color = "steelblue", linewidth = 0.5) +
    geom_point(data = anomalies, aes(x = date, y = daily_revenue),
               color = "red", size = 3) +
    labs(title = "Daily Revenue with Anomaly Detection",
         x = "Date", y = "Revenue (USD)") +
    theme_minimal()

  return(p)
}
