//! Perform frontend analysis:
//! * Binding analysis (connect variables to declarations and give each a unique name)
//! * Type checking (check the type usage and attach type information to AST nodes)

use std::collections::HashMap;

use crate::ast::{
    ActionDecl, Argument, Assignment, BaseType, BlockStatement, ConstantDecl, ControlDecl,
    ControlLocalDecl, Declaration, Expr, FunctionCall, IfStatement, Instantiation, KeyElement,
    Param, Program, Statement, StatementOrDecl, TableDecl, TableProperty, TypeRef, VariableDecl,
};
use crate::ir::{
    IrActionDecl, IrArgument, IrAssignment, IrBaseType, IrBlockStatement, IrControlDecl,
    IrControlLocalDecl, IrDeclaration, IrExpr, IrExprData, IrFunctionCall, IrFunctionType,
    IrIfStatement, IrInstantiation, IrKeyElement, IrParam, IrProgram, IrStatement,
    IrStatementOrDecl, IrStructDecl, IrStructType, IrTableDecl, IrTableProperty, IrType,
    IrVariableDecl, VariableId,
};

#[derive(Debug)]
pub enum TypeCheckError {
    /// The declaration of this variable was not found
    UnknownVar(String),
    /// There is more than one declaration of this variable in the same scope
    DuplicateDecl(String),
    /// Expected one type but got another
    MismatchedTypes { expected: IrType, found: IrType },
    /// Expected a function, found other type
    NotAFunction { found: IrType },
    /// Expected an action, found other type
    NotAnAction { found: IrType },
}

/// Run binding analysis on the program, creating a new program with unique
/// variable names given to each variable and a map from new name to ID.
pub fn run_type_checking(
    program: &Program,
) -> Result<(IrProgram, ProgramMetadata), TypeCheckError> {
    let mut env = EnvironmentStack::new();
    let new_program = program.type_check(&mut env)?;

    Ok((new_program, env.into()))
}

/// Holds some metadata about the program, such as the type of each variable.
pub struct ProgramMetadata {
    pub var_types: HashMap<VariableId, IrType>,
}

impl From<EnvironmentStack> for ProgramMetadata {
    fn from(env: EnvironmentStack) -> Self {
        Self {
            var_types: env.var_tys,
        }
    }
}

/// Maps AST identifiers such as variable names to semantic information for
/// items declared in a specific scope. For example:
/// * Variable name to ID
#[derive(Default)]
struct Environment {
    variables: HashMap<String, VariableId>,
}

/// Maps AST identifiers such as variable names to semantic information via a
/// stack of environments. This stack represents the various levels of scope at
/// a specific point in the program.
#[derive(Default)]
struct EnvironmentStack {
    stack: Vec<Environment>,
    var_tys: HashMap<VariableId, IrType>,
    next_id: usize,
}

impl EnvironmentStack {
    fn new() -> Self {
        Self::default()
    }

    /// Get the ID and type of the variable
    fn get_var(&self, name: &str) -> Option<(VariableId, &IrType)> {
        let id = self
            .stack
            .iter()
            .rev()
            .filter_map(|env| env.variables.get(name))
            .copied()
            .next()?;
        let ty = self.var_tys.get(&id)?;

        Some((id, ty))
    }

    fn get_var_or_err(&self, name: &str) -> Result<(VariableId, &IrType), TypeCheckError> {
        self.get_var(name)
            .ok_or_else(|| TypeCheckError::UnknownVar(name.to_string()))
    }

    /// Insert a variable into the environment and return a unique ID for it.
    /// If the variable has already been declared in this same scope, an
    /// error is returned.
    fn insert(&mut self, name: String, ty: IrType) -> Result<VariableId, TypeCheckError> {
        if self.stack.is_empty() {
            self.stack.push(Environment::default());
        }

        let env = self.stack.last_mut().unwrap();

        if env.variables.contains_key(&name) {
            return Err(TypeCheckError::DuplicateDecl(name));
        }

        let id = VariableId(self.next_id);
        self.next_id += 1;
        self.var_tys.insert(id, ty);
        env.variables.insert(name, id);

        Ok(id)
    }

    /// Push a scope (new environment) onto the stack
    fn push_scope(&mut self) {
        self.stack.push(Environment::default());
    }

    /// Pop a scope (environment) from the stack
    fn pop_scope(&mut self) {
        self.stack.pop();
    }
}

/// Trait for performing type checking and binding analysis on an AST node while
/// transforming it into typed IR.
trait TypeCheck: Sized {
    type IrNode;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError>;
}

impl<T: TypeCheck> TypeCheck for Vec<T> {
    type IrNode = Vec<T::IrNode>;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        let items = self
            .iter()
            .map(|item| item.type_check(env))
            .collect::<Result<_, _>>()?;

        Ok(items)
    }
}

impl<T: TypeCheck> TypeCheck for Option<T> {
    type IrNode = Option<T::IrNode>;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        self.as_ref().map(|inner| inner.type_check(env)).transpose()
    }
}

impl TypeCheck for Program {
    type IrNode = IrProgram;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        Ok(IrProgram {
            declarations: self.declarations.type_check(env)?,
        })
    }
}

impl TypeCheck for Declaration {
    type IrNode = IrDeclaration;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        match self {
            Declaration::Struct(_struct_decl) => {
                // TODO: handle structs
                Ok(IrDeclaration::Struct(IrStructDecl))
            }
            Declaration::Control(control_decl) => {
                Ok(IrDeclaration::Control(control_decl.type_check(env)?))
            }
            Declaration::Constant(const_decl) => {
                Ok(IrDeclaration::Constant(const_decl.type_check(env)?))
            }
            Declaration::Instantiation(instantiation) => {
                Ok(IrDeclaration::Instantiation(instantiation.type_check(env)?))
            }
        }
    }
}

impl TypeCheck for ControlDecl {
    type IrNode = IrControlDecl;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        // TODO: check name against types

        env.push_scope();
        let params = self.params.type_check(env)?;
        let local_decls = self.local_decls.type_check(env)?;
        let apply_body = self.apply_body.type_check(env)?;
        env.pop_scope();

        Ok(IrControlDecl {
            // name: self.name.clone(),
            params,
            local_decls,
            apply_body,
        })
    }
}

impl TypeCheck for Param {
    type IrNode = IrParam;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        let ty = self.ty.type_check(env)?;
        let id = env.insert(self.name.clone(), ty.clone())?;

        Ok(IrParam {
            ty,
            id,
            direction: self.direction,
        })
    }
}

impl TypeCheck for ControlLocalDecl {
    type IrNode = IrControlLocalDecl;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        match self {
            ControlLocalDecl::Variable(var_decl) => {
                Ok(IrControlLocalDecl::Variable(var_decl.type_check(env)?))
            }
            ControlLocalDecl::Instantiation(instantiation) => Ok(
                IrControlLocalDecl::Instantiation(instantiation.type_check(env)?),
            ),
            ControlLocalDecl::Constant(const_decl) => {
                Ok(IrControlLocalDecl::Variable(const_decl.type_check(env)?))
            }
            ControlLocalDecl::Action(action_decl) => {
                Ok(IrControlLocalDecl::Action(action_decl.type_check(env)?))
            }
            ControlLocalDecl::Table(table_decl) => {
                Ok(IrControlLocalDecl::Table(table_decl.type_check(env)?))
            }
        }
    }
}

impl TypeCheck for StatementOrDecl {
    type IrNode = IrStatementOrDecl;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        match self {
            StatementOrDecl::Statement(stmt) => {
                Ok(IrStatementOrDecl::Statement(stmt.type_check(env)?))
            }
            StatementOrDecl::VariableDecl(var_decl) => {
                Ok(IrStatementOrDecl::VariableDecl(var_decl.type_check(env)?))
            }
            StatementOrDecl::ConstantDecl(const_decl) => {
                Ok(IrStatementOrDecl::VariableDecl(const_decl.type_check(env)?))
            }
            StatementOrDecl::Instantiation(instantiation) => Ok(IrStatementOrDecl::Instantiation(
                instantiation.type_check(env)?,
            )),
        }
    }
}

impl TypeCheck for Statement {
    type IrNode = IrStatement;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        match self {
            Statement::Block(block) => Ok(IrStatement::Block(block.type_check(env)?)),
            Statement::If(if_stmt) => Ok(IrStatement::If(if_stmt.type_check(env)?)),
            Statement::Assignment(assignment) => {
                Ok(IrStatement::Assignment(assignment.type_check(env)?))
            }
            Statement::FunctionCall(func_call) => {
                Ok(IrStatement::FunctionCall(func_call.type_check(env)?))
            }
        }
    }
}

impl TypeCheck for BlockStatement {
    type IrNode = IrBlockStatement;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        env.push_scope();
        let stmts = self.0.type_check(env)?;
        env.pop_scope();

        Ok(IrBlockStatement(stmts))
    }
}

impl TypeCheck for ActionDecl {
    type IrNode = IrActionDecl;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        env.push_scope();
        let params = self.params.type_check(env)?;
        let body = self.body.type_check(env)?;
        env.pop_scope();

        let ty = IrFunctionType {
            result: Box::new(IrType::Base(IrBaseType::Void)),
            inputs: params
                .iter()
                .map(|param| (param.ty.clone(), param.direction))
                .collect(),
        };
        let id = env.insert(self.name.clone(), IrType::Function(ty.clone()))?;

        Ok(IrActionDecl {
            ty,
            id,
            params,
            body,
        })
    }
}

impl TypeCheck for TableDecl {
    type IrNode = IrTableDecl;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        let properties = self.properties.type_check(env)?;
        let id = env.insert(self.name.clone(), IrType::Base(IrBaseType::Table))?;

        Ok(IrTableDecl { id, properties })
    }
}

impl TypeCheck for TableProperty {
    type IrNode = IrTableProperty;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        match self {
            TableProperty::Key(keys) => Ok(IrTableProperty::Key(keys.type_check(env)?)),
            TableProperty::Actions(actions) => Ok(IrTableProperty::Actions(
                actions
                    .iter()
                    .map(|action| {
                        let (id, ty) = env.get_var_or_err(action)?;

                        match ty {
                            IrType::Function(IrFunctionType { result, .. })
                                if matches!(result.as_ref(), IrType::Base(IrBaseType::Void)) =>
                            {
                                Ok(id)
                            }
                            _ => Err(TypeCheckError::NotAnAction { found: ty.clone() }),
                        }
                    })
                    .collect::<Result<_, _>>()?,
            )),
        }
    }
}

impl TypeCheck for KeyElement {
    type IrNode = IrKeyElement;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        // Note: the "name" of the key is not to be modified. It refers to a key
        // type (ex. exact or lpm) and does not reference or declare a variable.
        // TODO: verify that the match kind has been declared previously
        Ok(IrKeyElement {
            match_kind: self.match_kind.clone(),
            expr: self.expr.type_check(env)?,
        })
    }
}

impl TypeCheck for TypeRef {
    type IrNode = IrType;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        match self {
            TypeRef::Base(base_ty) => Ok(IrType::Base(base_ty.type_check(env)?)),
            TypeRef::Identifier(name) => {
                // FIXME
                Ok(IrType::Struct(IrStructType { name: name.clone() }))
            }
        }
    }
}

impl TypeCheck for BaseType {
    type IrNode = IrBaseType;

    fn type_check(&self, _env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        match self {
            BaseType::Bool => Ok(IrBaseType::Bool),
        }
    }
}

impl TypeCheck for ConstantDecl {
    type IrNode = IrVariableDecl;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        let ty = self.ty.type_check(env)?;
        let value = self.value.type_check(env)?;
        let id = env.insert(self.name.clone(), ty.clone())?;

        assert_ty(&value.ty, &ty)?;

        Ok(IrVariableDecl {
            ty,
            id,
            value: Some(value),
            is_const: true,
        })
    }
}

impl TypeCheck for VariableDecl {
    type IrNode = IrVariableDecl;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        let ty = self.ty.type_check(env)?;
        let value = self.value.type_check(env)?;
        let id = env.insert(self.name.clone(), ty.clone())?;

        if let Some(value) = &value {
            assert_ty(&value.ty, &ty)?;
        }

        Ok(IrVariableDecl {
            ty,
            id,
            value,
            is_const: false,
        })
    }
}

impl TypeCheck for Instantiation {
    type IrNode = IrInstantiation;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        let ty = self.ty.type_check(env)?;
        let args = self.args.type_check(env)?;
        let id = env.insert(self.name.clone(), ty.clone())?;

        Ok(IrInstantiation { ty, id, args })
    }
}

impl TypeCheck for IfStatement {
    type IrNode = IrIfStatement;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        let condition = self.condition.type_check(env)?;
        let then_case = self.then_case.type_check(env)?;
        let else_case = self.else_case.type_check(env)?;

        assert_ty(&condition.ty, &IrType::bool())?;

        Ok(IrIfStatement {
            condition,
            then_case,
            else_case,
        })
    }
}

impl TypeCheck for Assignment {
    type IrNode = IrAssignment;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        let (id, ty) = env.get_var_or_err(&self.name)?;
        let ty = ty.clone();
        let value = self.value.type_check(env)?;

        assert_ty(&value.ty, &ty)?;

        Ok(IrAssignment { var: id, value })
    }
}

impl TypeCheck for FunctionCall {
    type IrNode = IrFunctionCall;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        let (target_id, target_ty) = env.get_var_or_err(&self.target)?;
        let target_ty = target_ty.clone();
        let arguments = self.arguments.type_check(env)?;

        let func_ty = match target_ty {
            IrType::Function(ty) => ty,
            _ => return Err(TypeCheckError::NotAFunction { found: target_ty }),
        };

        Ok(IrFunctionCall {
            result_ty: func_ty.result.as_ref().clone(),
            target: target_id,
            arguments,
        })
    }
}

impl TypeCheck for Argument {
    type IrNode = IrArgument;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        match self {
            Argument::Value(value) => Ok(IrArgument::Value(value.type_check(env)?)),
            Argument::Named(name, value) => Ok(IrArgument::Named(
                env.get_var_or_err(name)?.0,
                value.type_check(env)?,
            )),
            Argument::DontCare => Ok(IrArgument::DontCare),
        }
    }
}

fn assert_ty(found: &IrType, expected: &IrType) -> Result<(), TypeCheckError> {
    if found == expected {
        Ok(())
    } else {
        Err(TypeCheckError::MismatchedTypes {
            expected: expected.clone(),
            found: found.clone(),
        })
    }
}

impl TypeCheck for Expr {
    type IrNode = IrExpr;

    fn type_check(&self, env: &mut EnvironmentStack) -> Result<Self::IrNode, TypeCheckError> {
        match self {
            Expr::Bool(value) => Ok(IrExpr {
                ty: IrType::bool(),
                data: IrExprData::Bool(*value),
            }),
            Expr::Var(name) => {
                let (id, ty) = env.get_var_or_err(name)?;

                Ok(IrExpr {
                    ty: ty.clone(),
                    data: IrExprData::Var(id),
                })
            }
            Expr::And(left, right) => {
                let left_ir = left.type_check(env)?;
                let right_ir = right.type_check(env)?;

                assert_ty(&left_ir.ty, &IrType::bool())?;
                assert_ty(&right_ir.ty, &IrType::bool())?;

                Ok(IrExpr {
                    ty: IrType::bool(),
                    data: IrExprData::And(Box::new(left_ir), Box::new(right_ir)),
                })
            }
            Expr::Or(left, right) => {
                let left_ir = left.type_check(env)?;
                let right_ir = right.type_check(env)?;

                assert_ty(&left_ir.ty, &IrType::bool())?;
                assert_ty(&right_ir.ty, &IrType::bool())?;

                Ok(IrExpr {
                    ty: IrType::bool(),
                    data: IrExprData::Or(Box::new(left_ir), Box::new(right_ir)),
                })
            }
            Expr::Negation(inner) => {
                let inner_ir = inner.type_check(env)?;

                assert_ty(&inner_ir.ty, &IrType::bool())?;

                Ok(IrExpr {
                    ty: IrType::bool(),
                    data: IrExprData::Negation(Box::new(inner_ir)),
                })
            }
            Expr::FunctionCall(func_call) => {
                let func_call_ir = func_call.type_check(env)?;

                Ok(IrExpr {
                    ty: func_call_ir.result_ty.clone(),
                    data: IrExprData::FunctionCall(func_call_ir),
                })
            }
        }
    }
}
