use eframe::{egui, App, Frame, NativeOptions};
use egui::{Color32, Context, RichText, Visuals};
use egui_plot::{GridMark, Line, Plot, PlotPoints, Points, Text};
use polars::prelude::*;
use rfd::FileDialog;
use rusqlite::{params, Connection};
use std::time::{SystemTime, UNIX_EPOCH};
use chrono::TimeZone;

fn main() -> eframe::Result<()> {
    let options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 880.0])
            .with_min_inner_size([950.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Shielded Observer",
        options,
        Box::new(|cc| {
            apply_shielded_theme(&cc.egui_ctx, true);
            Box::new(ShieldedObserverApp::new(cc))
        }),
    )
}

struct ShieldedObserverApp {
    dark_mode: bool,
    ui_scale: f32,
    pending_scale: f32,
    show_settings: bool,
    status_message: String,
    last_update: String,
    city_input: String,
    state_input: String,
    search_result: Option<LocationResult>,
    validated_locations: Vec<LocationResult>,
    current_location: Option<LocationResult>,
    use_local_time: bool,
    show_export_window: bool,
    export_format: String,
}

#[derive(Clone, Debug)]
struct LocationResult {
    city: String,
    state: String,
    lat: f64,
    lon: f64,
}

const BASE_PLOT_HEIGHT: f32 = 450.0;

impl ShieldedObserverApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
    let conn = init_database().expect("Failed to initialize database");
    Self {
        dark_mode: true,
        ui_scale: 1.0,
        pending_scale: 1.0,
        show_settings: false,
        status_message: "Idle".to_string(),
        last_update: "Never".to_string(),
        city_input: String::new(),
        state_input: String::new(),
        search_result: None,
        validated_locations: load_validated_locations(&conn),
        current_location: None,
        use_local_time: true,
        show_export_window: false,
        export_format: "CSV".to_string(),
    }
}

    fn apply_scale(&mut self, ctx: &Context) {
        if (self.pending_scale - self.ui_scale).abs() > 0.001 {
            self.ui_scale = self.pending_scale;
            ctx.set_pixels_per_point(self.ui_scale);
        }
    }

    fn perform_export(&self, loc: &LocationResult) -> Result<String, String> {
        let conn = init_database().map_err(|e| e.to_string())?;

        let collected_at = get_latest_collection_time(&conn, loc)
            .map_err(|e| e.to_string())?
            .ok_or("No data collected for this location yet")?;

        // Query data for export
        let mut stmt = conn.prepare(
            "SELECT start_time, temperature, dewpoint, relative_humidity, wind_speed, wind_direction, short_forecast 
             FROM hourly_forecasts 
             WHERE city = ?1 AND state = ?2 AND collected_at = ?3 
             ORDER BY start_time ASC"
        ).map_err(|e| e.to_string())?;

        let mut rows = stmt.query(params![loc.city, loc.state, collected_at]).map_err(|e| e.to_string())?;

        let mut times = Vec::new();
        let mut temps = Vec::new();
        let mut dewpoints = Vec::new();
        let mut humidities = Vec::new();
        let mut wind_speeds = Vec::new();
        let mut wind_dirs = Vec::new();
        let mut forecasts = Vec::new();

        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            times.push(row.get::<_, String>(0).unwrap_or_default());
            temps.push(row.get::<_, Option<f64>>(1).unwrap_or(None));
            dewpoints.push(row.get::<_, Option<f64>>(2).unwrap_or(None));
            humidities.push(row.get::<_, Option<f64>>(3).unwrap_or(None));
            wind_speeds.push(row.get::<_, Option<String>>(4).unwrap_or(None));
            wind_dirs.push(row.get::<_, Option<String>>(5).unwrap_or(None));
            forecasts.push(row.get::<_, Option<String>>(6).unwrap_or(None));
        }

        if times.is_empty() {
            return Err("No data found for export".to_string());
        }

        // Create Polars DataFrame
        let df = DataFrame::new(vec![
            Series::new("start_time".into(), times),
            Series::new("temperature".into(), temps),
            Series::new("dewpoint".into(), dewpoints),
            Series::new("relative_humidity".into(), humidities),
            Series::new("wind_speed".into(), wind_speeds),
            Series::new("wind_direction".into(), wind_dirs),
            Series::new("short_forecast".into(), forecasts),
        ]).map_err(|e| e.to_string())?;

        // File dialog
        let file = FileDialog::new()
            .set_file_name(format!("{}_{}", loc.city, loc.state))
            .add_filter("CSV", &["csv"])
            .add_filter("JSON", &["json"])
            .save_file();

        let mut path = match file {
            Some(p) => p,
            None => return Err("No file selected".to_string()),
        };

        // Append correct extension based on selected format
        match self.export_format.as_str() {
            "CSV" => {
                if path.extension().map_or(true, |ext| !ext.eq_ignore_ascii_case("csv")) {
                    path.set_extension("csv");           // ← Line 150
                }
                let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                CsvWriter::new(&mut file)
                    .finish(&mut df.clone())
                    .map_err(|e| e.to_string())?;
            }
            "JSON" => {
                if path.extension().map_or(true, |ext| !ext.eq_ignore_ascii_case("json")) {
                    path.set_extension("json");          // ← Line 159
                }
                let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                JsonWriter::new(&mut file)
                    .finish(&mut df.clone())
                    .map_err(|e| e.to_string())?;
            }
            _ => return Err("PNG export not implemented yet".to_string()),
        }

        let path_str = path.to_string_lossy().to_string();

        // Write based on selected format
        match self.export_format.as_str() {
            "CSV" => {
                let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                CsvWriter::new(&mut file)
                    .finish(&mut df.clone())
                    .map_err(|e| e.to_string())?;
            }
            "JSON" => {
                let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                JsonWriter::new(&mut file)
                    .finish(&mut df.clone())
                    .map_err(|e| e.to_string())?;
            }
            _ => return Err("PNG export not implemented yet".to_string()),
        }

        Ok(path_str)
    }
}

impl App for ShieldedObserverApp {
    fn update(&mut self, ctx: &Context, _frame: &mut Frame) {
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Shielded Observer");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Export").clicked() {
                        self.show_export_window = true;
                    }
                    if ui.button("⚙ Settings").clicked() {
                        self.show_settings = !self.show_settings;
                    }
                    let theme_label = if self.dark_mode { "☀ Light" } else { "🌙 Dark" };
                    if ui.button(theme_label).clicked() {
                        self.dark_mode = !self.dark_mode;
                        apply_shielded_theme(ctx, self.dark_mode);
                    }
                });
            });
        });

        egui::SidePanel::left("sidebar").resizable(true).show(ctx, |ui| {
            ui.heading("📍 Locations");
            ui.separator();
            ui.label("Search City, State");
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut self.city_input);
                ui.text_edit_singleline(&mut self.state_input);
            });
            if ui.button("Search").clicked() {
                if !self.city_input.is_empty() && !self.state_input.is_empty() {
                    match geocode_location(&self.city_input, &self.state_input) {
                        Ok(result) => {
                            self.search_result = Some(result.clone());
                            self.status_message = "Location found".to_string();
                        }
                        Err(e) => {
                            self.status_message = format!("Error: {}", e);
                        }
                    }
                }
            }
            ui.add_space(8.0);
            if let Some(result) = &self.search_result {
                ui.label(format!("{}, {}", result.city, result.state));
                ui.label(format!("Lat: {:.4} Lon: {:.4}", result.lat, result.lon));
                if ui.button("Use this location").clicked() {
                    self.current_location = Some(result.clone());
                    self.status_message = format!("Selected: {}, {}", result.city, result.state);
                }
                if ui.button("Save to validated locations").clicked() {
                    if let Err(e) = save_validated_location(result) {
                        self.status_message = format!("Failed to save: {}", e);
                    } else {
                        self.validated_locations = load_validated_locations(&init_database().unwrap());
                        self.status_message = "Location saved".to_string();
                    }
                }
            }
            ui.separator();
            ui.label("Validated Locations");
            for (i, loc) in self.validated_locations.clone().iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.selectable_label(false, format!("{}, {}", loc.city, loc.state)).clicked() {
                        self.current_location = Some(loc.clone());
                        self.status_message = format!("Selected: {}, {}", loc.city, loc.state);
                    }
                    if ui.small_button("×").clicked() {
                        if let Err(e) = delete_validated_location(&loc.city, &loc.state) {
                            self.status_message = format!("Delete failed: {}", e);
                        } else {
                            self.validated_locations.remove(i);
                            self.status_message = "Location removed".to_string();
                        }
                    }
                });
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Main Content Area");
            if let Some(loc) = &self.current_location {
                ui.label(format!("Current Location: {}, {}", loc.city, loc.state));
                ui.label(format!("Coordinates: {:.4}, {:.4}", loc.lat, loc.lon));
                ui.add_space(15.0);
                if ui.button("Collect Now").clicked() {
                    match collect_hourly_forecast(loc) {
                        Ok(count) => {
                            self.status_message = format!("Collected {} hourly periods", count);
                            self.last_update = chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string();
                        }
                        Err(e) => {
                            self.status_message = format!("Collection failed: {}", e);
                        }
                    }
                }
                ui.add_space(25.0);

                let scale = self.ui_scale;
                let plot_height = BASE_PLOT_HEIGHT / scale;
                let label_size = 12.0 / scale;
                let use_local = self.use_local_time;

                // Temperature + Dewpoint
                ui.heading("Temperature + Dewpoint (°F)");
                match get_temp_and_dewpoint_data(loc) {
                    Ok((temp_points, dew_points)) => {
                        if !temp_points.is_empty() || !dew_points.is_empty() {
                            Plot::new("temp_dew_plot")
                                .height(plot_height)
                                .allow_drag(false)
                                .allow_zoom(false)
                                .allow_scroll(false)
                                .allow_boxed_zoom(false)
                                .x_axis_formatter(move |mark: GridMark, _step: usize, range: &std::ops::RangeInclusive<f64>| {
                                    format_time_label(mark, range, use_local)
                                })
                                .show(ui, |plot_ui| {
                                    if !temp_points.is_empty() {
                                        let line = Line::new(PlotPoints::from_iter(temp_points.iter().copied()))
                                            .color(Color32::from_rgb(220, 50, 50))
                                            .width(2.5);
                                        plot_ui.line(line);
                                        for (i, point) in temp_points.iter().enumerate() {
                                            plot_ui.points(Points::new(PlotPoints::new(vec![*point])).radius(3.5).color(Color32::from_rgb(220, 50, 50)));
                                            if i % 4 == 0 {
                                                plot_ui.text(
                                                    Text::new(
                                                        egui_plot::PlotPoint::new(point[0], point[1] + 2.5),
                                                        RichText::new(format!("{:.0}", point[1])).size(label_size)
                                                    )
                                                        .color(Color32::from_rgb(220, 50, 50))
                                                        .anchor(egui::Align2::CENTER_BOTTOM)
                                                );
                                            }
                                        }
                                    }
                                    if !dew_points.is_empty() {
                                        let line = Line::new(PlotPoints::from_iter(dew_points.iter().copied()))
                                            .color(Color32::from_rgb(50, 180, 80))
                                            .width(2.5);
                                        plot_ui.line(line);
                                        for (i, point) in dew_points.iter().enumerate() {
                                            plot_ui.points(Points::new(PlotPoints::new(vec![*point])).radius(3.0).color(Color32::from_rgb(50, 180, 80)));
                                            if i % 4 == 0 {
                                                plot_ui.text(
                                                    Text::new(
                                                        egui_plot::PlotPoint::new(point[0], point[1] + 2.0),
                                                        RichText::new(format!("{:.0}", point[1])).size(label_size)
                                                    )
                                                        .color(Color32::from_rgb(50, 180, 80))
                                                        .anchor(egui::Align2::CENTER_BOTTOM)
                                                );
                                            }
                                        }
                                    }
                                });
                        } else {
                            ui.label("No temperature/dewpoint data yet. Click 'Collect Now'.");
                        }
                    }
                    Err(_) => {
                        ui.label("Failed to load temperature data.");
                    }
                }
                ui.add_space(15.0);

                // Wind Speed
                ui.heading("Wind Speed (mph)");
                match get_wind_speed_data(loc) {
                    Ok(points) => {
                        if !points.is_empty() {
                            Plot::new("wind_plot")
                                .height(plot_height)
                                .allow_drag(false)
                                .allow_zoom(false)
                                .allow_scroll(false)
                                .allow_boxed_zoom(false)
                                .x_axis_formatter(move |mark: GridMark, _step: usize, range: &std::ops::RangeInclusive<f64>| {
                                    format_time_label(mark, range, use_local)
                                })
                                .show(ui, |plot_ui| {
                                    plot_with_labels(plot_ui, &points, Color32::from_rgb(50, 120, 200), true, label_size);
                                });
                        } else {
                            ui.label("No wind speed data yet. Click 'Collect Now'.");
                        }
                    }
                    Err(_) => {
                        ui.label("Failed to load wind speed data.");
                    }
                }
                ui.add_space(15.0);

                // Relative Humidity
                ui.heading("Relative Humidity (%)");
                match get_relative_humidity_data(loc) {
                    Ok(points) => {
                        if !points.is_empty() {
                            Plot::new("humidity_plot")
                                .height(plot_height)
                                .allow_drag(false)
                                .allow_zoom(false)
                                .allow_scroll(false)
                                .allow_boxed_zoom(false)
                                .x_axis_formatter(move |mark: GridMark, _step: usize, range: &std::ops::RangeInclusive<f64>| {
                                    format_time_label(mark, range, use_local)
                                })
                                .show(ui, |plot_ui| {
                                    plot_with_labels(plot_ui, &points, Color32::from_rgb(80, 160, 220), true, label_size);
                                });
                        } else {
                            ui.label("No humidity data yet. Click 'Collect Now'.");
                        }
                    }
                    Err(_) => {
                        ui.label("Failed to load humidity data.");
                    }
                }
            } else {
                ui.label("No location selected yet.");
            }
        });

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Status: {}", self.status_message));
                ui.separator();
                ui.label(format!("Last Update: {}", self.last_update));
                ui.separator();
                ui.label("Errors: 0");
                ui.separator();
                ui.label("Freshness: —");
                ui.separator();
                ui.label("Mode: Manual");
            });
        });

        let mut apply_scale_now = false;
        if self.show_settings {
            egui::Window::new("Settings")
                .open(&mut self.show_settings)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.heading("Appearance");
                    ui.horizontal(|ui| {
                        ui.label("UI Scale:");
                        let response = ui.add(egui::Slider::new(&mut self.pending_scale, 0.75..=2.0).step_by(0.05).text("x"));
                        if response.drag_stopped() || response.lost_focus() {
                            apply_scale_now = true;
                        }
                    });
                    ui.label("Theme");
                    if ui.radio_value(&mut self.dark_mode, true, "Dark (Shielded)").clicked() {
                        apply_shielded_theme(ctx, true);
                    }
                    if ui.radio_value(&mut self.dark_mode, false, "Light").clicked() {
                        apply_shielded_theme(ctx, false);
                    }
                    ui.add_space(10.0);
                    ui.separator();
                    ui.label("Graph Timezone");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.use_local_time, true, "Local Time");
                        ui.radio_value(&mut self.use_local_time, false, "UTC");
                    });
                    ui.label("Affects x-axis time labels (AM/PM)");
                });
        }
        if apply_scale_now {
            self.apply_scale(ctx);
        }

        // Export Window
        if self.show_export_window {
            let mut export_open = self.show_export_window;
            let mut should_close = false;

            egui::Window::new("Export Data")
                .open(&mut export_open)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.heading("Export Options");

                    ui.horizontal(|ui| {
                        ui.label("Format:");
                        egui::ComboBox::from_label("")
                            .selected_text(&self.export_format)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.export_format, "CSV".to_string(), "CSV");
                                ui.selectable_value(&mut self.export_format, "JSON".to_string(), "JSON");
                                ui.selectable_value(&mut self.export_format, "PNG (coming soon)".to_string(), "PNG (coming soon)");
                            });
                    });

                    ui.add_space(10.0);

                    if ui.button("Choose location & Export").clicked() {
                        if let Some(loc) = &self.current_location {
                            match self.perform_export(loc) {
                                Ok(path) => {
                                    self.status_message = format!("Exported to {}", path);
                                    should_close = true;
                                }
                                Err(e) => {
                                    self.status_message = format!("Export failed: {}", e);
                                }
                            }
                        } else {
                            self.status_message = "No location selected".to_string();
                        }
                    }

                    ui.label("Note: PNG graph export coming soon.");
                });

            if should_close {
                self.show_export_window = false;
            } else {
                self.show_export_window = export_open;
            }
        }
    }
}

// ==================== Helper Functions ====================
fn format_time_label(mark: GridMark, _range: &std::ops::RangeInclusive<f64>, use_local: bool) -> String {
    let ts = mark.value as i64;
    let dt_utc = match chrono::Utc.timestamp_opt(ts, 0).single() {
        Some(d) => d,
        None => return String::new(),
    };
    if use_local {
        let dt = dt_utc.with_timezone(&chrono::Local);
        let mut s = dt.format("%-I%P").to_string();
        s.make_ascii_lowercase();
        s
    } else {
        let mut s = dt_utc.format("%-I%P").to_string();
        s.make_ascii_lowercase();
        s
    }
}

fn plot_with_labels(plot_ui: &mut egui_plot::PlotUi, points: &[[f64; 2]], color: Color32, show_labels: bool, label_size: f32) {
    if points.is_empty() { return; }
    let line = Line::new(PlotPoints::from_iter(points.iter().copied())).color(color).width(2.0);
    plot_ui.line(line);
    for (i, point) in points.iter().enumerate() {
        plot_ui.points(Points::new(PlotPoints::new(vec![*point])).radius(3.5).color(color));
        if show_labels && i % 3 == 0 {
            plot_ui.text(
                Text::new(
                    egui_plot::PlotPoint::new(point[0], point[1] + 2.0),
                    RichText::new(format!("{:.0}", point[1])).size(label_size)
                )
                    .color(color)
                    .anchor(egui::Align2::CENTER_BOTTOM)
            );
        }
    }
}

// ==================== Data Access Helpers ====================
fn get_latest_collection_time(conn: &Connection, loc: &LocationResult) -> rusqlite::Result<Option<i64>> {
    let mut stmt = conn.prepare("SELECT MAX(collected_at) FROM hourly_forecasts WHERE city = ?1 AND state = ?2")?;
    stmt.query_row(params![loc.city, loc.state], |row| row.get(0))
}

fn get_temp_and_dewpoint_data(loc: &LocationResult) -> Result<(Vec<[f64; 2]>, Vec<[f64; 2]>), Box<dyn std::error::Error>> {
    let conn = init_database()?;
    let collected_at = match get_latest_collection_time(&conn, loc)? {
        Some(t) => t,
        None => return Ok((vec![], vec![])),
    };
    let mut stmt = conn.prepare("SELECT start_time, temperature, dewpoint FROM hourly_forecasts WHERE city = ?1 AND state = ?2 AND collected_at = ?3 ORDER BY start_time ASC")?;
    let mut rows = stmt.query(params![loc.city, loc.state, collected_at])?;
    let mut temp_points = Vec::new();
    let mut dew_points = Vec::new();
    while let Some(row) = rows.next()? {
        let time_str: String = row.get(0)?;
        let temp: Option<f64> = row.get(1)?;
        let dew: Option<f64> = row.get(2)?;
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&time_str) {
            let dt = dt.with_timezone(&chrono::Utc);
            let x = dt.timestamp() as f64;
            temp_points.push([x, temp.unwrap_or(0.0)]);
            dew_points.push([x, dew.unwrap_or(0.0)]);
        }
    }
    Ok((temp_points, dew_points))
}

fn get_wind_speed_data(loc: &LocationResult) -> Result<Vec<[f64; 2]>, Box<dyn std::error::Error>> {
    let conn = init_database()?;
    let collected_at = match get_latest_collection_time(&conn, loc)? {
        Some(t) => t,
        None => return Ok(vec![]),
    };
    let mut stmt = conn.prepare("SELECT start_time, wind_speed FROM hourly_forecasts WHERE city = ?1 AND state = ?2 AND collected_at = ?3 ORDER BY start_time ASC")?;
    let mut rows = stmt.query(params![loc.city, loc.state, collected_at])?;
    let mut points = Vec::new();
    while let Some(row) = rows.next()? {
        let time_str: String = row.get(0)?;
        let wind_str: Option<String> = row.get(1)?;
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&time_str) {
            let dt = dt.with_timezone(&chrono::Utc);
            let x = dt.timestamp() as f64;
            let speed = wind_str.as_deref().and_then(parse_wind_speed).unwrap_or(0.0);
            points.push([x, speed]);
        }
    }
    Ok(points)
}

fn get_relative_humidity_data(loc: &LocationResult) -> Result<Vec<[f64; 2]>, Box<dyn std::error::Error>> {
    let conn = init_database()?;
    let collected_at = match get_latest_collection_time(&conn, loc)? {
        Some(t) => t,
        None => return Ok(vec![]),
    };
    let mut stmt = conn.prepare("SELECT start_time, relative_humidity FROM hourly_forecasts WHERE city = ?1 AND state = ?2 AND collected_at = ?3 ORDER BY start_time ASC")?;
    let mut rows = stmt.query(params![loc.city, loc.state, collected_at])?;
    let mut points = Vec::new();
    while let Some(row) = rows.next()? {
        let time_str: String = row.get(0)?;
        let humidity: Option<f64> = row.get(1)?;
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&time_str) {
            let dt = dt.with_timezone(&chrono::Utc);
            let x = dt.timestamp() as f64;
            points.push([x, humidity.unwrap_or(0.0)]);
        }
    }
    Ok(points)
}

fn parse_wind_speed(s: &str) -> Option<f64> {
    s.split_whitespace().find_map(|part| part.parse::<f64>().ok())
}

// ==================== Database & Collection ====================
fn init_database() -> rusqlite::Result<Connection> {
    let conn = Connection::open("shielded_observer.db")?;
    conn.execute("CREATE TABLE IF NOT EXISTS validated_locations (id INTEGER PRIMARY KEY, city TEXT NOT NULL, state TEXT NOT NULL, lat REAL NOT NULL, lon REAL NOT NULL, last_validated INTEGER NOT NULL, UNIQUE(city, state))", [])?;
    conn.execute("CREATE TABLE IF NOT EXISTS hourly_forecasts (id INTEGER PRIMARY KEY, city TEXT NOT NULL, state TEXT NOT NULL, start_time TEXT NOT NULL, temperature REAL, dewpoint REAL, relative_humidity REAL, wind_speed TEXT, wind_direction TEXT, pop REAL, short_forecast TEXT, collected_at INTEGER NOT NULL)", [])?;
    Ok(conn)
}

fn load_validated_locations(conn: &Connection) -> Vec<LocationResult> {
    let mut stmt = conn.prepare("SELECT city, state, lat, lon FROM validated_locations ORDER BY last_validated DESC").unwrap();
    stmt.query_map([], |row| Ok(LocationResult { city: row.get(0)?, state: row.get(1)?, lat: row.get(2)?, lon: row.get(3)? })).unwrap().filter_map(Result::ok).collect()
}

fn save_validated_location(loc: &LocationResult) -> rusqlite::Result<()> {
    let conn = init_database()?;
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    conn.execute("INSERT INTO validated_locations (city, state, lat, lon, last_validated) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(city, state) DO UPDATE SET lat = excluded.lat, lon = excluded.lon, last_validated = excluded.last_validated", params![loc.city, loc.state, loc.lat, loc.lon, timestamp])?;
    Ok(())
}

fn delete_validated_location(city: &str, state: &str) -> rusqlite::Result<()> {
    let conn = init_database()?;
    conn.execute("DELETE FROM validated_locations WHERE city = ?1 AND state = ?2", params![city, state])?;
    Ok(())
}

fn collect_hourly_forecast(loc: &LocationResult) -> Result<usize, String> {
    let point_url = format!("https://api.weather.gov/points/{},{}", loc.lat, loc.lon);
    let client = reqwest::blocking::Client::builder()
        .user_agent("shielded-observer/0.1 (https://github.com/dismad/shielded-observer)")
        .build()
        .map_err(|e| e.to_string())?;
    let point_resp: serde_json::Value = client.get(&point_url).send().map_err(|e| e.to_string())?.json().map_err(|e| e.to_string())?;
    let forecast_url = point_resp["properties"]["forecastHourly"].as_str().ok_or("Could not find forecastHourly URL")?;
    let forecast_resp: serde_json::Value = client.get(forecast_url).send().map_err(|e| e.to_string())?.json().map_err(|e| e.to_string())?;
    let periods = forecast_resp["properties"]["periods"].as_array().ok_or("No periods found")?;
    let conn = init_database().map_err(|e| e.to_string())?;
    let collected_at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let mut count = 0;
    for period in periods {
        let temp = period["temperature"].as_f64();
        let dewpoint_c = period["dewpoint"]["value"].as_f64();
        let dewpoint_f = dewpoint_c.map(|c| c * 9.0 / 5.0 + 32.0);
        let humidity = period["relativeHumidity"]["value"].as_f64();
        conn.execute(
            "INSERT INTO hourly_forecasts (city, state, start_time, temperature, dewpoint, relative_humidity, wind_speed, wind_direction, pop, short_forecast, collected_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![loc.city, loc.state, period["startTime"].as_str().unwrap_or(""), temp, dewpoint_f, humidity, period["windSpeed"].as_str(), period["windDirection"].as_str(), period["probabilityOfPrecipitation"]["value"].as_f64(), period["shortForecast"].as_str(), collected_at],
        ).map_err(|e| e.to_string())?;
        count += 1;
    }
    Ok(count)
}

fn geocode_location(city: &str, state: &str) -> Result<LocationResult, String> {
    if let Ok(conn) = init_database() {
        if let Ok(Some(result)) = check_cache(&conn, city, state) { return Ok(result); }
    }
    let query = format!("{}, {}", city, state);
    let url = format!("https://nominatim.openstreetmap.org/search?format=json&q={}&limit=1", urlencoding::encode(&query));
    let client = reqwest::blocking::Client::builder()
        .user_agent("shielded-observer/0.1 (https://github.com/dismad/shielded-observer)")
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(&url).send().map_err(|e| e.to_string())?;
    if !response.status().is_success() { return Err(format!("Nominatim error: {}", response.status())); }
    let text = response.text().map_err(|e| e.to_string())?;
    let results: Vec<serde_json::Value> = serde_json::from_str(&text).map_err(|_| "Failed to parse JSON".to_string())?;
    if let Some(first) = results.first() {
        let lat: f64 = first["lat"].as_str().unwrap().parse().unwrap();
        let lon: f64 = first["lon"].as_str().unwrap().parse().unwrap();
        let result = LocationResult { city: city.to_string(), state: state.to_string(), lat, lon };
        let _ = save_validated_location(&result);
        Ok(result)
    } else {
        Err("No results found".to_string())
    }
}

fn check_cache(conn: &Connection, city: &str, state: &str) -> rusqlite::Result<Option<LocationResult>> {
    let mut stmt = conn.prepare("SELECT lat, lon FROM validated_locations WHERE city = ?1 AND state = ?2 LIMIT 1")?;
    let mut rows = stmt.query(params![city, state])?;
    if let Some(row) = rows.next()? {
        Ok(Some(LocationResult { city: city.to_string(), state: state.to_string(), lat: row.get(0)?, lon: row.get(1)? }))
    } else {
        Ok(None)
    }
}

fn apply_shielded_theme(ctx: &Context, dark: bool) {
    let mut visuals = if dark { Visuals::dark() } else { Visuals::light() };
    if dark {
        visuals.panel_fill = Color32::from_rgb(20, 28, 40);
        visuals.window_fill = Color32::from_rgb(25, 35, 50);
        visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(30, 42, 58);
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(35, 50, 70);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(45, 65, 90);
        visuals.widgets.active.bg_fill = Color32::from_rgb(55, 80, 110);
    } else {
        visuals.panel_fill = Color32::from_rgb(240, 245, 252);
        visuals.window_fill = Color32::from_rgb(245, 248, 255);
    }
    ctx.set_visuals(visuals);
}