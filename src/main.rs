#[macro_use]
extern crate lalrpop_util;

use crate::ast::Program;
use crate::gcl::{GclExpr, GclGraph, GclNode};
use crate::generate_z3_types::{generate_types, Z3TypeMap};
use crate::lexer::{LalrpopLexerIter, Token};
use crate::optimizations::merge_simple_edges;
use crate::to_gcl::ToGcl;
use crate::to_predicates::{PredicateMap, VariableMap};
use crate::type_checker::run_type_checking;
use env_logger::Env;
use lalrpop_util::ParseError;
use logos::Logos;
use petgraph::dot::Dot;
use petgraph::graph::NodeIndex;
use petgraph::visit::IntoNodeReferences;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::ops::Deref;
use std::time::Instant;
use z3::ast::Bool;
use z3::{Config, Context, Model, SatResult, Solver};

mod ast;
mod gcl;
mod generate_z3_types;
mod ir;
mod lexer;
mod optimizations;
mod to_gcl;
mod to_predicates;
mod type_checker;
mod to_z3;

lalrpop_mod!(
    #[allow(clippy::all)]
    p4_parser
);

fn main() {
    env_logger::Builder::from_env(Env::default().default_filter_or("info"))
        .format(|buf, record| writeln!(buf, "[{}] {}", record.level(), record.args()))
        .init();

    let args: Vec<String> = std::env::args().collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    // Only check reachability of bug nodes by default
    let mut only_bugs = true;

    match args.as_slice() {
        [_, "--full-reachability"] => only_bugs = false,
        [_] | [] => {}
        [name, ..] => {
            eprintln!("Usage: {} [--full-reachability]", name);
            return;
        }
    }

    // Read P4 program
    let mut p4_program_str = String::new();
    std::io::stdin()
        .read_to_string(&mut p4_program_str)
        .unwrap();

    // Parse P4
    let parse_start = Instant::now();
    let p4_program = parse(&p4_program_str);
    let time_to_parse = parse_start.elapsed();

    // Type check P4
    let type_checking_start = Instant::now();
    let (p4_program_ir, metadata) = run_type_checking(&p4_program).unwrap();
    let time_to_type_check = type_checking_start.elapsed();
    log::trace!("After type checking: {:#?}", p4_program_ir);

    // Convert to GCL
    let gcl_start = Instant::now();
    let mut graph = GclGraph::new();
    let gcl_start_node = p4_program_ir.to_gcl(&mut graph, &metadata);
    let time_to_gcl = gcl_start.elapsed();

    // Optimize GCL
    let gcl_optimize_start = Instant::now();
    merge_simple_edges(&mut graph);
    let time_to_optimize_gcl = gcl_optimize_start.elapsed();

    // Calculate a reachability predicate for each node
    let reachability_start = Instant::now();
    let (node_predicates, node_variables) = graph.to_reachability_predicates();
    let time_to_reachability = reachability_start.elapsed();
    display_node_vars(&graph, &node_variables);
    display_reachability(&graph, &node_predicates);

    // Convert predicates to Z3
    let z3_convert_start = Instant::now();
    let z3_config = Config::new();
    let z3_context = Context::new(&z3_config);
    let z3_types = generate_types(&metadata.types_in_order, &z3_context);
    let z3_predicates = convert_to_z3(&graph, &node_predicates, &z3_context, &z3_types, only_bugs);
    let time_to_convert_z3 = z3_convert_start.elapsed();

    // Calculate reachability
    let reachable_start = Instant::now();
    let is_reachable = calculate_reachable(z3_predicates, &z3_context);
    let time_to_reachable = reachable_start.elapsed();

    // Print out the graphviz representation
    let graphviz = make_graphviz(&graph, &is_reachable);
    log::info!("{}", graphviz);

    // Show all reachable bugs
    display_bugs(&graph, &is_reachable, gcl_start_node);

    log::info!(
        "Time to parse P4: {}ms\n\
         Time to type check: {}ms\n\
         Time to convert to GCL: {}ms\n\
         Time to optimize GCL: {}ms\n\
         Time to build reachability predicates: {}ms\n\
         Time to convert to Z3: {}ms\n\
         Time to calculate reachability: {}ms\n\
         Total time: {}ms",
        time_to_parse.as_millis(),
        time_to_type_check.as_millis(),
        time_to_gcl.as_millis(),
        time_to_optimize_gcl.as_millis(),
        time_to_reachability.as_millis(),
        time_to_convert_z3.as_millis(),
        time_to_reachable.as_millis(),
        parse_start.elapsed().as_millis()
    );
}

fn display_reachability(graph: &GclGraph, node_preds: &PredicateMap) {
    log::debug!("Reachability Predicates:");
    for (node_idx, pred) in node_preds {
        let node_name = &graph.node_weight(*node_idx).unwrap().name;

        log::debug!("Node '{}': {}", node_name, pred);
    }
}

fn display_node_vars(graph: &GclGraph, node_vars: &VariableMap) {
    log::trace!("Node Variables:");
    let mut node_vars: Vec<_> = node_vars
        .iter()
        .map(|(node_idx, values)| (graph.node_weight(*node_idx).unwrap().name.as_str(), values))
        .collect();
    node_vars.sort_by_key(|(name, _)| *name);

    for (node_name, vars) in node_vars {
        log::trace!("Node '{}':", node_name);
        for (var, values) in vars {
            log::trace!(
                "    {} = [{}]",
                var,
                values
                    .iter()
                    .map(|v| format!("{}", v))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
}

fn convert_to_z3<'ctx>(
    graph: &GclGraph,
    node_predicates: &HashMap<NodeIndex, GclExpr>,
    context: &'ctx Context,
    type_map: &Z3TypeMap<'ctx>,
    only_bugs: bool,
) -> HashMap<NodeIndex, Bool<'ctx>> {
    graph
        .node_references()
        .filter_map(|(node_idx, node)| {
            if only_bugs && !node.is_bug() {
                return None;
            }

            let pred = node_predicates.get(&node_idx).unwrap();
            Some((
                node_idx,
                pred.as_z3_ast(&context, &type_map).as_bool().unwrap(),
            ))
        })
        .collect()
}

fn calculate_reachable<'ctx>(
    z3_predicates: HashMap<NodeIndex, Bool<'ctx>>,
    context: &'ctx Context,
) -> HashMap<NodeIndex, Option<Model<'ctx>>> {
    let solver = Solver::new(&context);

    z3_predicates
        .into_iter()
        .map(|(node_idx, z3_pred)| {
            let z3_result = solver.check_assumptions(&[z3_pred]);
            if z3_result == SatResult::Sat {
                let model = solver.get_model().unwrap();

                (node_idx, Some(model))
            } else {
                (node_idx, None)
            }
        })
        .collect()
}

fn make_graphviz(graph: &GclGraph, is_reachable: &HashMap<NodeIndex, Option<Model>>) -> String {
    let get_node_attributes = |_graph, (node_idx, node): (NodeIndex, &GclNode)| {
        let color = match (node.is_bug(), is_reachable.get(&node_idx)) {
            (true, Some(Some(_))) => "red",
            (false, Some(Some(_))) => "green",
            (_, Some(None)) => "grey",
            (_, None) => "black",
        };

        format!("shape = box, color = {}", color)
    };
    let graphviz_graph = Dot::with_attr_getters(
        graph.deref(),
        &[],
        &|_graph, _edge| String::new(),
        &get_node_attributes,
    );

    graphviz_graph.to_string()
}

fn display_bugs(
    graph: &GclGraph,
    is_reachable: &HashMap<NodeIndex, Option<Model>>,
    start_idx: NodeIndex,
) {
    let mut found_bug = false;

    for (node_idx, node) in graph.node_references() {
        if !node.is_bug() {
            continue;
        }

        let model = match is_reachable.get(&node_idx).unwrap_or(&None) {
            Some(model) => model,
            None => continue,
        };

        found_bug = true;
        let path = path_to(graph, start_idx, node_idx).map(|path| {
            // Get the name of each node
            path.into_iter()
                .map(|node_idx| graph.node_weight(node_idx).unwrap().name.as_str())
                .collect::<Vec<_>>()
        });
        log::info!(
            "Found bug: {:?}\nPath = {:?}\nModel = {}",
            node,
            path,
            model
        );
    }

    if !found_bug {
        log::info!("No bugs found!");
    }
}

fn path_to(graph: &GclGraph, start_idx: NodeIndex, node_idx: NodeIndex) -> Option<Vec<NodeIndex>> {
    petgraph::algo::all_simple_paths(graph.deref(), start_idx, node_idx, 0, None).next()
}

/// Parse the P4 program. If there are errors during parsing, the program will
/// exit.
fn parse(p4_program_str: &str) -> Program {
    let lexer_state = RefCell::default();
    let lexer = Token::lexer_with_extras(p4_program_str, &lexer_state);
    let lexer_iter = LalrpopLexerIter::new(lexer);

    match p4_parser::ProgramParser::new().parse(p4_program_str, &lexer_state, lexer_iter) {
        Ok(parsed_ast) => {
            log::trace!("Parsed AST: {:#?}\n", parsed_ast);
            parsed_ast
        }
        Err(ParseError::InvalidToken { location }) => {
            let (line, col) = index_to_line_col(p4_program_str, location);
            log::error!("Invalid token at line {}, column {}", line, col);
            std::process::exit(1);
        }
        Err(ParseError::UnrecognizedToken {
            token: (lspan, token, _rspan),
            expected,
        }) => {
            let (line, col) = index_to_line_col(p4_program_str, lspan);
            log::error!(
                "Unrecognized token '{:?}' at line {}, column {}, expected [{}]",
                token,
                line,
                col,
                expected.join(", ")
            );
            std::process::exit(1);
        }
        Err(ParseError::UnrecognizedEOF { location, expected }) => {
            let (line, col) = index_to_line_col(p4_program_str, location);
            log::error!(
                "Unexpected EOF at line {}, column {}, expected [{}]",
                line,
                col,
                expected.join(", ")
            );
            std::process::exit(1);
        }
        Err(ParseError::ExtraToken {
            token: (lspan, token, _rspan),
        }) => {
            let (line, col) = index_to_line_col(p4_program_str, lspan);
            log::error!(
                "Unexpected extra token '{:?}' at line {}, column {}",
                token,
                line,
                col
            );
            std::process::exit(1);
        }
        Err(ParseError::User { error }) => {
            let token = &p4_program_str[error.clone()];
            let (line, col) = index_to_line_col(p4_program_str, error.start);
            log::error!("Invalid token '{}' at line {}, column {}", token, line, col);
            std::process::exit(1);
        }
    }
}

/// Convert an index of the file into a line and column index
fn index_to_line_col(file_str: &str, index: usize) -> (usize, usize) {
    let line = file_str
        .chars()
        .enumerate()
        .take_while(|(i, _)| *i != index)
        .filter(|(_, c)| *c == '\n')
        .count()
        + 1;
    let column = file_str[0..index]
        .chars()
        .rev()
        .take_while(|c| *c != '\n')
        .count()
        + 1;

    (line, column)
}
