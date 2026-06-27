use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Tabs, Wrap},
    Frame,
};

use crate::client::fe::QueryResult;

use super::app::{App, Tab};

pub fn render(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(2),
        ])
        .split(frame.area());

    render_header(frame, app, chunks[0]);
    render_tabs(frame, app, chunks[1]);
    render_body(frame, app, chunks[2]);
    render_footer(frame, app, chunks[3]);
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let summary = app
        .snapshot
        .as_ref()
        .map(|s| {
            format!(
                "FE {}/{} alive | BE {}/{} alive | decom {} | tablet problem rows {}",
                s.summary.fe_alive,
                s.summary.fe_total,
                s.summary.be_alive,
                s.summary.be_total,
                s.summary.be_decommissioning,
                s.summary.tablet_problem_rows
            )
        })
        .unwrap_or_else(|| "snapshot unavailable".to_string());
    let text = vec![Line::from(vec![
        Span::styled(
            "dcli TUI",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  cluster={}  fe={}:{}  {}",
            app.cfg.name, app.cfg.fe.host, app.cfg.fe.query_port, summary
        )),
    ])];
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn render_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles = Tab::ALL
        .iter()
        .map(|tab| Line::from(tab.title()))
        .collect::<Vec<_>>();
    let tabs = Tabs::new(titles)
        .select(app.active_tab)
        .block(Block::default().borders(Borders::ALL))
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, area);
}

fn render_body(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.tab() {
        Tab::Overview => render_overview(frame, app, area),
        Tab::Frontends => render_result_table(
            frame,
            area,
            "SHOW FRONTENDS",
            app.snapshot.as_ref().map(|s| &s.frontends),
            app.table_offset,
        ),
        Tab::Backends => render_result_table(
            frame,
            area,
            "SHOW BACKENDS",
            app.snapshot.as_ref().map(|s| &s.backends),
            app.table_offset,
        ),
        Tab::Sql => render_sql(frame, app, area),
        Tab::Ops => render_ops(frame, app, area),
        Tab::Logs => render_placeholder(
            frame,
            area,
            "Logs",
            "Log analysis is reserved for the next TUI slice.",
        ),
    }
}

fn render_overview(frame: &mut Frame, app: &App, area: Rect) {
    let Some(snapshot) = &app.snapshot else {
        render_placeholder(frame, area, "Overview", "Loading cluster snapshot...");
        return;
    };
    let rows = vec![
        Row::new(vec![
            Cell::from("Frontends"),
            Cell::from(format!(
                "{}/{} alive",
                snapshot.summary.fe_alive, snapshot.summary.fe_total
            )),
        ]),
        Row::new(vec![
            Cell::from("Backends"),
            Cell::from(format!(
                "{}/{} alive, {} decommissioning",
                snapshot.summary.be_alive,
                snapshot.summary.be_total,
                snapshot.summary.be_decommissioning
            )),
        ]),
        Row::new(vec![
            Cell::from("Tablet health"),
            Cell::from(format!(
                "{} db row(s), {} problem row(s)",
                snapshot.summary.tablet_health_rows, snapshot.summary.tablet_problem_rows
            )),
        ]),
    ];
    let table = Table::new(rows, [Constraint::Length(18), Constraint::Min(20)])
        .block(Block::default().title("Overview").borders(Borders::ALL));
    frame.render_widget(table, area);
}

fn render_ops(frame: &mut Frame, app: &App, area: Rect) {
    render_result_table(
        frame,
        area,
        "Tablet health",
        app.snapshot.as_ref().map(|s| &s.tablet_health),
        app.table_offset,
    );
}

fn render_sql(frame: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(area);
    app.sql
        .editor
        .set_block(Block::default().title("SQL").borders(Borders::ALL));
    frame.render_widget(&app.sql.editor, chunks[0]);
    render_result_table(
        frame,
        chunks[1],
        "Result",
        app.sql.result.as_ref(),
        app.sql.result_offset,
    );
}

fn render_result_table(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    result: Option<&QueryResult>,
    offset: usize,
) {
    let Some(result) = result else {
        render_placeholder(frame, area, title, "No data");
        return;
    };
    if result.columns.is_empty() {
        render_placeholder(frame, area, title, "No rows");
        return;
    }

    let widths = result
        .columns
        .iter()
        .map(|_| Constraint::Min(12))
        .collect::<Vec<_>>();
    let header = Row::new(
        result
            .columns
            .iter()
            .map(|c| Cell::from(c.clone()).style(Style::default().fg(Color::Cyan))),
    )
    .style(Style::default().add_modifier(Modifier::BOLD));
    let rows = result
        .rows
        .iter()
        .skip(offset)
        .map(|row| Row::new(row.iter().map(|cell| Cell::from(cell.clone()))));
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().title(title).borders(Borders::ALL))
        .column_spacing(1);
    frame.render_widget(table, area);
}

fn render_placeholder(frame: &mut Frame, area: Rect, title: &str, message: &str) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(message.to_string())
            .block(Block::default().title(title).borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let help = match app.tab() {
        Tab::Sql => "Tab/Arrows switch | Ctrl+Enter run SQL | Alt+Up/Down result scroll | r refresh | q quit",
        _ => "Tab/Arrows switch | j/k scroll | r or F5 refresh | q quit",
    };
    let text = vec![Line::from(vec![
        Span::styled(help, Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(&app.status, Style::default().fg(Color::Green)),
    ])];
    frame.render_widget(Paragraph::new(text), area);
}
