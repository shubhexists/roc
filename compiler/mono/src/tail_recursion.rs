use crate::ir::{CallType, Expr, JoinPointId, Param, Stmt};
use crate::layout::Layout;
use bumpalo::collections::Vec;
use bumpalo::Bump;
use roc_module::symbol::Symbol;

/// Make tail calls into loops (using join points)
///
/// e.g.
///
/// > factorial n accum = if n == 1 then accum else factorial (n - 1) (n * accum)
///
/// becomes
///
/// ```elm
/// factorial n1 accum1 =
///     let joinpoint j n accum =
///             if n == 1 then
///                 accum
///             else
///                 jump j (n - 1) (n * accum)
///
///     in
///         jump j n1 accum1
/// ```
///
/// This will effectively compile into a loop in llvm, and
/// won't grow the call stack for each iteration
pub fn make_tail_recursive<'a>(
    arena: &'a Bump,
    id: JoinPointId,
    needle: Symbol,
    stmt: Stmt<'a>,
    args: &'a [(Layout<'a>, Symbol)],
) -> Stmt<'a> {
    let alloced = arena.alloc(stmt);
    match insert_jumps(arena, alloced, id, needle) {
        None => alloced.clone(),
        Some(new) => {
            // jumps were inserted, we must now add a join point

            let params = Vec::from_iter_in(
                args.iter().map(|(layout, symbol)| Param {
                    symbol: *symbol,
                    layout: layout.clone(),
                    borrow: true,
                }),
                arena,
            )
            .into_bump_slice();

            // TODO could this be &[]?
            let args = Vec::from_iter_in(args.iter().map(|t| t.1), arena).into_bump_slice();

            let jump = arena.alloc(Stmt::Jump(id, args));

            Stmt::Join {
                id,
                remainder: jump,
                parameters: params,
                continuation: new,
            }
        }
    }
}

fn insert_jumps<'a>(
    arena: &'a Bump,
    stmt: &'a Stmt<'a>,
    goal_id: JoinPointId,
    needle: Symbol,
) -> Option<&'a Stmt<'a>> {
    use Stmt::*;

    match stmt {
        Let(
            symbol,
            Expr::Call(crate::ir::Call {
                call_type: CallType::ByName { name: fsym, .. },
                arguments,
                ..
            }),
            _,
            Stmt::Ret(rsym),
        ) if needle == *fsym && symbol == rsym => {
            // replace the call and return with a jump

            let jump = Stmt::Jump(goal_id, arguments);

            Some(arena.alloc(jump))
        }

        Invoke {
            symbol,
            call:
                crate::ir::Call {
                    call_type: CallType::ByName { name: fsym, .. },
                    arguments,
                    ..
                },
            fail,
            pass: Stmt::Ret(rsym),
            ..
        } if needle == *fsym && symbol == rsym => {
            debug_assert_eq!(fail, &&Stmt::Rethrow);

            // replace the call and return with a jump

            let jump = Stmt::Jump(goal_id, arguments);

            Some(arena.alloc(jump))
        }

        Let(symbol, expr, layout, cont) => {
            let opt_cont = insert_jumps(arena, cont, goal_id, needle);

            if opt_cont.is_some() {
                let cont = opt_cont.unwrap_or(cont);

                Some(arena.alloc(Let(*symbol, expr.clone(), layout.clone(), cont)))
            } else {
                None
            }
        }

        Invoke {
            symbol,
            call,
            fail,
            pass,
            layout,
        } => {
            let opt_pass = insert_jumps(arena, pass, goal_id, needle);
            let opt_fail = insert_jumps(arena, fail, goal_id, needle);

            if opt_pass.is_some() || opt_fail.is_some() {
                let pass = opt_pass.unwrap_or(pass);
                let fail = opt_fail.unwrap_or(fail);

                let stmt = Invoke {
                    symbol: *symbol,
                    call: call.clone(),
                    layout: layout.clone(),
                    pass,
                    fail,
                };

                Some(arena.alloc(stmt))
            } else {
                None
            }
        }

        Join {
            id,
            parameters,
            remainder,
            continuation,
        } => {
            let opt_remainder = insert_jumps(arena, remainder, goal_id, needle);
            let opt_continuation = insert_jumps(arena, continuation, goal_id, needle);

            if opt_remainder.is_some() || opt_continuation.is_some() {
                let remainder = opt_remainder.unwrap_or(remainder);
                let continuation = opt_continuation.unwrap_or_else(|| *continuation);

                Some(arena.alloc(Join {
                    id: *id,
                    parameters,
                    remainder,
                    continuation,
                }))
            } else {
                None
            }
        }
        Switch {
            cond_symbol,
            cond_layout,
            branches,
            default_branch,
            ret_layout,
        } => {
            let opt_default = insert_jumps(arena, default_branch.1, goal_id, needle);

            let mut did_change = false;

            let opt_branches = Vec::from_iter_in(
                branches.iter().map(|(label, info, branch)| {
                    match insert_jumps(arena, branch, goal_id, needle) {
                        None => None,
                        Some(branch) => {
                            did_change = true;
                            Some((*label, info.clone(), branch.clone()))
                        }
                    }
                }),
                arena,
            );

            if opt_default.is_some() || did_change {
                let default_branch = (
                    default_branch.0.clone(),
                    opt_default.unwrap_or(default_branch.1),
                );

                let branches = if did_change {
                    let new = Vec::from_iter_in(
                        opt_branches.into_iter().zip(branches.iter()).map(
                            |(opt_branch, branch)| match opt_branch {
                                None => branch.clone(),
                                Some(new_branch) => new_branch,
                            },
                        ),
                        arena,
                    );

                    new.into_bump_slice()
                } else {
                    branches
                };

                Some(arena.alloc(Switch {
                    cond_symbol: *cond_symbol,
                    cond_layout: cond_layout.clone(),
                    default_branch,
                    branches,
                    ret_layout: ret_layout.clone(),
                }))
            } else {
                None
            }
        }
        Refcounting(modify, cont) => match insert_jumps(arena, cont, goal_id, needle) {
            Some(cont) => Some(arena.alloc(Refcounting(*modify, cont))),
            None => None,
        },

        Rethrow => None,
        Ret(_) => None,
        Jump(_, _) => None,
        RuntimeError(_) => None,
    }
}
