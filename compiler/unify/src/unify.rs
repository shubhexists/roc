use bitflags::bitflags;
use roc_error_macros::internal_error;
use roc_module::ident::{Lowercase, TagName};
use roc_module::symbol::Symbol;
use roc_types::subs::Content::{self, *};
use roc_types::subs::{
    AliasVariables, Descriptor, ErrorTypeContext, FlatType, GetSubsSlice, Mark, OptVariable,
    RecordFields, Subs, SubsFmtContent, SubsIndex, SubsSlice, UnionTags, Variable,
    VariableSubsSlice,
};
use roc_types::types::{AliasKind, DoesNotImplementAbility, ErrorType, Mismatch, RecordField};

macro_rules! mismatch {
    () => {{
        if cfg!(debug_assertions) && std::env::var("ROC_PRINT_MISMATCHES").is_ok() {
            println!(
                "Mismatch in {} Line {} Column {}",
                file!(),
                line!(),
                column!()
            );
        }

        Outcome {
            mismatches: vec![Mismatch::TypeMismatch],
            ..Outcome::default()
        }
    }};
    ($msg:expr) => {{
        if cfg!(debug_assertions) && std::env::var("ROC_PRINT_MISMATCHES").is_ok() {
            println!(
                "Mismatch in {} Line {} Column {}",
                file!(),
                line!(),
                column!()
            );
            println!($msg);
            println!("");
        }


        Outcome {
            mismatches: vec![Mismatch::TypeMismatch],
            ..Outcome::default()
        }
    }};
    ($msg:expr,) => {{
        mismatch!($msg)
    }};
    ($msg:expr, $($arg:tt)*) => {{
        if cfg!(debug_assertions) && std::env::var("ROC_PRINT_MISMATCHES").is_ok() {
            println!(
                "Mismatch in {} Line {} Column {}",
                file!(),
                line!(),
                column!()
            );
            println!($msg, $($arg)*);
            println!("");
        }

        Outcome {
            mismatches: vec![Mismatch::TypeMismatch],
            ..Outcome::default()
        }
    }};
    (%not_able, $var:expr, $ability:expr, $msg:expr, $($arg:tt)*) => {{
        if cfg!(debug_assertions) && std::env::var("ROC_PRINT_MISMATCHES").is_ok() {
            println!(
                "Mismatch in {} Line {} Column {}",
                file!(),
                line!(),
                column!()
            );
            println!($msg, $($arg)*);
            println!("");
        }

        Outcome {
            mismatches: vec![Mismatch::TypeMismatch, Mismatch::DoesNotImplementAbiity($var, $ability)],
            ..Outcome::default()
        }
    }}
}

type Pool = Vec<Variable>;

bitflags! {
    pub struct Mode : u8 {
        /// Instructs the unifier to solve two types for equality.
        ///
        /// For example, { n : Str }a ~ { n: Str, m : Str } will solve "a" to "{ m : Str }".
        const EQ = 1 << 0;
        /// Instructs the unifier to treat the right-hand-side of a constraint as
        /// present in the left-hand-side, rather than strictly equal.
        ///
        /// For example, t1 += [A Str] says we should "add" the tag "A Str" to the type of "t1".
        const PRESENT = 1 << 1;
        /// Instructs the unifier to treat rigids exactly like flex vars.
        /// Usually rigids can only unify with flex vars, because rigids are named and bound
        /// explicitly.
        /// However, when checking type ranges, as we do for `RangedNumber` types, we must loosen
        /// this restriction because otherwise an admissible range will appear inadmissible.
        /// For example, Int * is in the range <I8, U8, ...>.
        const RIGID_AS_FLEX = 1 << 2;
    }
}

impl Mode {
    fn is_eq(&self) -> bool {
        debug_assert!(!self.contains(Mode::EQ | Mode::PRESENT));
        self.contains(Mode::EQ)
    }

    fn is_present(&self) -> bool {
        debug_assert!(!self.contains(Mode::EQ | Mode::PRESENT));
        self.contains(Mode::PRESENT)
    }

    fn as_eq(self) -> Self {
        (self - Mode::PRESENT) | Mode::EQ
    }
}

#[derive(Debug)]
pub struct Context {
    first: Variable,
    first_desc: Descriptor,
    second: Variable,
    second_desc: Descriptor,
    mode: Mode,
}

#[derive(Debug)]
pub enum Unified {
    Success {
        vars: Pool,
        must_implement_ability: MustImplementConstraints,
    },
    Failure(Pool, ErrorType, ErrorType, DoesNotImplementAbility),
    BadType(Pool, roc_types::types::Problem),
}

impl Unified {
    pub fn expect_success(self, err_msg: &'static str) -> (Pool, MustImplementConstraints) {
        match self {
            Unified::Success {
                vars,
                must_implement_ability,
            } => (vars, must_implement_ability),
            _ => internal_error!("{}", err_msg),
        }
    }
}

/// Specifies that `type` must implement the ability `ability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MustImplementAbility {
    // This only points to opaque type names currently.
    // TODO(abilities) support structural types in general
    pub typ: Symbol,
    pub ability: Symbol,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MustImplementConstraints(Vec<MustImplementAbility>);

impl MustImplementConstraints {
    pub fn push(&mut self, must_implement: MustImplementAbility) {
        self.0.push(must_implement)
    }

    pub fn extend(&mut self, other: Self) {
        self.0.extend(other.0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get_unique(mut self) -> Vec<MustImplementAbility> {
        self.0.sort();
        self.0.dedup();
        self.0
    }

    pub fn iter_for_ability(&self, ability: Symbol) -> impl Iterator<Item = &MustImplementAbility> {
        self.0.iter().filter(move |mia| mia.ability == ability)
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    mismatches: Vec<Mismatch>,
    /// We defer these checks until the end of a solving phase.
    /// NOTE: this vector is almost always empty!
    must_implement_ability: MustImplementConstraints,
}

impl Outcome {
    fn union(&mut self, other: Self) {
        self.mismatches.extend(other.mismatches);
        self.must_implement_ability
            .extend(other.must_implement_ability);
    }
}

#[inline(always)]
pub fn unify(subs: &mut Subs, var1: Variable, var2: Variable, mode: Mode) -> Unified {
    let mut vars = Vec::new();
    let Outcome {
        mismatches,
        must_implement_ability,
    } = unify_pool(subs, &mut vars, var1, var2, mode);

    if mismatches.is_empty() {
        Unified::Success {
            vars,
            must_implement_ability,
        }
    } else {
        let error_context = if mismatches.contains(&Mismatch::TypeNotInRange) {
            ErrorTypeContext::ExpandRanges
        } else {
            ErrorTypeContext::None
        };

        let (type1, mut problems) = subs.var_to_error_type_contextual(var1, error_context);
        let (type2, problems2) = subs.var_to_error_type_contextual(var2, error_context);

        problems.extend(problems2);

        subs.union(var1, var2, Content::Error.into());

        if !problems.is_empty() {
            Unified::BadType(vars, problems.remove(0))
        } else {
            let do_not_implement_ability = mismatches
                .into_iter()
                .filter_map(|mismatch| match mismatch {
                    Mismatch::DoesNotImplementAbiity(var, ab) => {
                        let (err_type, _new_problems) =
                            subs.var_to_error_type_contextual(var, error_context);
                        Some((err_type, ab))
                    }
                    _ => None,
                })
                .collect();

            Unified::Failure(vars, type1, type2, do_not_implement_ability)
        }
    }
}

#[inline(always)]
pub fn unify_pool(
    subs: &mut Subs,
    pool: &mut Pool,
    var1: Variable,
    var2: Variable,
    mode: Mode,
) -> Outcome {
    if subs.equivalent(var1, var2) {
        Outcome::default()
    } else {
        let ctx = Context {
            first: var1,
            first_desc: subs.get(var1),
            second: var2,
            second_desc: subs.get(var2),
            mode,
        };

        unify_context(subs, pool, ctx)
    }
}

/// Set `ROC_PRINT_UNIFICATIONS` in debug runs to print unifications as they start and complete as
/// a tree to stderr.
/// NOTE: Only run this on individual tests! Run on multiple threads, this would clobber each others' output.
#[cfg(debug_assertions)]
fn debug_print_unified_types(subs: &mut Subs, ctx: &Context, opt_outcome: Option<&Outcome>) {
    static mut UNIFICATION_DEPTH: usize = 0;

    if std::env::var("ROC_PRINT_UNIFICATIONS").is_ok() {
        let prefix = match opt_outcome {
            None => "❔",
            Some(outcome) if outcome.mismatches.is_empty() => "✅",
            Some(_) => "❌",
        };

        let depth = unsafe { UNIFICATION_DEPTH };
        let indent = 2;
        let (use_depth, new_depth) = if opt_outcome.is_none() {
            (depth, depth + indent)
        } else {
            (depth - indent, depth - indent)
        };

        // NOTE: names are generated here (when creating an error type) and that modifies names
        // generated by pretty_print.rs. So many test will fail with changes in variable names when
        // this block runs.
        //        let (type1, _problems1) = subs.var_to_error_type(ctx.first);
        //        let (type2, _problems2) = subs.var_to_error_type(ctx.second);
        //        println!("\n --------------- \n");
        //        dbg!(ctx.first, type1);
        //        println!("\n --- \n");
        //        dbg!(ctx.second, type2);
        //        println!("\n --------------- \n");
        let content_1 = subs.get(ctx.first).content;
        let content_2 = subs.get(ctx.second).content;
        let mode = if ctx.mode.is_eq() { "~" } else { "+=" };
        eprintln!(
            "{}{}({:?}-{:?}): {:?} {:?} {} {:?} {:?}",
            " ".repeat(use_depth),
            prefix,
            ctx.first,
            ctx.second,
            ctx.first,
            SubsFmtContent(&content_1, subs),
            mode,
            ctx.second,
            SubsFmtContent(&content_2, subs),
        );

        unsafe { UNIFICATION_DEPTH = new_depth };
    }
}

fn unify_context(subs: &mut Subs, pool: &mut Pool, ctx: Context) -> Outcome {
    #[cfg(debug_assertions)]
    debug_print_unified_types(subs, &ctx, None);

    let result = match &ctx.first_desc.content {
        FlexVar(opt_name) => unify_flex(subs, &ctx, opt_name, None, &ctx.second_desc.content),
        FlexAbleVar(opt_name, ability) => unify_flex(
            subs,
            &ctx,
            opt_name,
            Some(*ability),
            &ctx.second_desc.content,
        ),
        RecursionVar {
            opt_name,
            structure,
        } => unify_recursion(
            subs,
            pool,
            &ctx,
            opt_name,
            *structure,
            &ctx.second_desc.content,
        ),
        RigidVar(name) => unify_rigid(subs, &ctx, name, None, &ctx.second_desc.content),
        RigidAbleVar(name, ability) => {
            unify_rigid(subs, &ctx, name, Some(*ability), &ctx.second_desc.content)
        }
        Structure(flat_type) => {
            unify_structure(subs, pool, &ctx, flat_type, &ctx.second_desc.content)
        }
        Alias(symbol, args, real_var, kind) => {
            unify_alias(subs, pool, &ctx, *symbol, *args, *real_var, *kind)
        }
        &RangedNumber(typ, range_vars) => unify_ranged_number(subs, pool, &ctx, typ, range_vars),
        Error => {
            // Error propagates. Whatever we're comparing it to doesn't matter!
            merge(subs, &ctx, Error)
        }
    };

    #[cfg(debug_assertions)]
    debug_print_unified_types(subs, &ctx, Some(&result));

    result
}

#[inline(always)]
fn unify_ranged_number(
    subs: &mut Subs,
    pool: &mut Pool,
    ctx: &Context,
    real_var: Variable,
    range_vars: VariableSubsSlice,
) -> Outcome {
    let other_content = &ctx.second_desc.content;

    let outcome = match other_content {
        FlexVar(_) => {
            // Ranged number wins
            merge(subs, ctx, RangedNumber(real_var, range_vars))
        }
        RecursionVar { .. }
        | RigidVar(..)
        | Alias(..)
        | Structure(..)
        | RigidAbleVar(..)
        | FlexAbleVar(..) => unify_pool(subs, pool, real_var, ctx.second, ctx.mode),
        &RangedNumber(other_real_var, other_range_vars) => {
            let outcome = unify_pool(subs, pool, real_var, other_real_var, ctx.mode);
            if outcome.mismatches.is_empty() {
                check_valid_range(subs, pool, ctx.first, other_range_vars, ctx.mode)
            } else {
                outcome
            }
            // TODO: We should probably check that "range_vars" and "other_range_vars" intersect
        }
        Error => merge(subs, ctx, Error),
    };

    if !outcome.mismatches.is_empty() {
        return outcome;
    }

    check_valid_range(subs, pool, ctx.second, range_vars, ctx.mode)
}

fn check_valid_range(
    subs: &mut Subs,
    pool: &mut Pool,
    var: Variable,
    range: VariableSubsSlice,
    mode: Mode,
) -> Outcome {
    let slice = subs.get_subs_slice(range).to_vec();

    let mut it = slice.iter().peekable();
    while let Some(&possible_var) = it.next() {
        let snapshot = subs.snapshot();
        let old_pool = pool.clone();
        let outcome = unify_pool(subs, pool, var, possible_var, mode | Mode::RIGID_AS_FLEX);
        if outcome.mismatches.is_empty() {
            // Okay, we matched some type in the range.
            subs.rollback_to(snapshot);
            *pool = old_pool;
            return Outcome::default();
        } else if it.peek().is_some() {
            // We failed to match something in the range, but there are still things we can try.
            subs.rollback_to(snapshot);
            *pool = old_pool;
        } else {
            subs.commit_snapshot(snapshot);
        }
    }

    Outcome {
        mismatches: vec![Mismatch::TypeNotInRange],
        ..Outcome::default()
    }
}

#[inline(always)]
fn unify_alias(
    subs: &mut Subs,
    pool: &mut Pool,
    ctx: &Context,
    symbol: Symbol,
    args: AliasVariables,
    real_var: Variable,
    kind: AliasKind,
) -> Outcome {
    let other_content = &ctx.second_desc.content;

    let either_is_opaque =
        kind == AliasKind::Opaque || matches!(other_content, Alias(_, _, _, AliasKind::Opaque));

    match other_content {
        FlexVar(_) => {
            // Alias wins
            merge(subs, ctx, Alias(symbol, args, real_var, kind))
        }
        RecursionVar { structure, .. } if !either_is_opaque => {
            unify_pool(subs, pool, real_var, *structure, ctx.mode)
        }
        RigidVar(_) | RigidAbleVar(..) => unify_pool(subs, pool, real_var, ctx.second, ctx.mode),
        FlexAbleVar(_, ability) if kind == AliasKind::Opaque && args.is_empty() => {
            // Opaque type wins
            let mut outcome = merge(subs, ctx, Alias(symbol, args, real_var, kind));
            outcome.must_implement_ability.push(MustImplementAbility { typ: symbol, ability: *ability });
            outcome
        }
        Alias(other_symbol, other_args, other_real_var, _)
            // Opaques types are only equal if the opaque symbols are equal!
            if !either_is_opaque || symbol == *other_symbol =>
        {
            if symbol == *other_symbol {
                if args.len() == other_args.len() {
                    let mut outcome = Outcome::default();
                    let it = args
                        .all_variables()
                        .into_iter()
                        .zip(other_args.all_variables().into_iter());

                    let args_unification_snapshot = subs.snapshot();

                    for (l, r) in it {
                        let l_var = subs[l];
                        let r_var = subs[r];
                        outcome.union(unify_pool(subs, pool, l_var, r_var, ctx.mode));
                    }

                    if outcome.mismatches.is_empty() {
                        outcome.union(merge(subs, ctx, *other_content));
                    }

                    let args_unification_had_changes = !subs.vars_since_snapshot(&args_unification_snapshot).is_empty();
                    subs.commit_snapshot(args_unification_snapshot);

                    if !args.is_empty() && args_unification_had_changes && outcome.mismatches.is_empty() {
                        // We need to unify the real vars because unification of type variables
                        // may have made them larger, which then needs to be reflected in the `real_var`.
                        outcome.union(unify_pool(subs, pool, real_var, *other_real_var, ctx.mode));
                    }

                    outcome
                } else {
                    dbg!(args.len(), other_args.len());
                    mismatch!("{:?}", symbol)
                }
            } else {
                unify_pool(subs, pool, real_var, *other_real_var, ctx.mode)
            }
        }
        Structure(_) if !either_is_opaque => unify_pool(subs, pool, real_var, ctx.second, ctx.mode),
        RangedNumber(other_real_var, other_range_vars) if !either_is_opaque => {
            let outcome = unify_pool(subs, pool, real_var, *other_real_var, ctx.mode);
            if outcome.mismatches.is_empty() {
                check_valid_range(subs, pool, real_var, *other_range_vars, ctx.mode)
            } else {
                outcome
            }
        }
        Error => merge(subs, ctx, Error),
        other => {
            // The type on the left is an alias, but the one on the right is not!
            debug_assert!(either_is_opaque);
            mismatch!("Cannot unify opaque {:?} with {:?}", symbol, other)
        }
    }
}

#[inline(always)]
fn unify_structure(
    subs: &mut Subs,
    pool: &mut Pool,
    ctx: &Context,
    flat_type: &FlatType,
    other: &Content,
) -> Outcome {
    match other {
        FlexVar(_) => {
            // If the other is flex, Structure wins!
            let outcome = merge(subs, ctx, Structure(*flat_type));

            // And if we see a flex variable on the right hand side of a presence
            // constraint, we know we need to open up the structure we're trying to unify with.
            match (ctx.mode.is_present(), flat_type) {
                (true, FlatType::TagUnion(tags, _ext)) => {
                    let new_ext = subs.fresh_unnamed_flex_var();
                    let mut new_desc = ctx.first_desc;
                    new_desc.content = Structure(FlatType::TagUnion(*tags, new_ext));
                    subs.set(ctx.first, new_desc);
                }
                (true, FlatType::FunctionOrTagUnion(tn, sym, _ext)) => {
                    let new_ext = subs.fresh_unnamed_flex_var();
                    let mut new_desc = ctx.first_desc;
                    new_desc.content = Structure(FlatType::FunctionOrTagUnion(*tn, *sym, new_ext));
                    subs.set(ctx.first, new_desc);
                }
                _ => {}
            }
            outcome
        }
        RigidVar(name) => {
            // Type mismatch! Rigid can only unify with flex.
            mismatch!("trying to unify {:?} with rigid var {:?}", &flat_type, name)
        }
        RecursionVar { structure, .. } => match flat_type {
            FlatType::TagUnion(_, _) => {
                // unify the structure with this unrecursive tag union
                let mut outcome = unify_pool(subs, pool, ctx.first, *structure, ctx.mode);

                if outcome.mismatches.is_empty() {
                    outcome.union(fix_tag_union_recursion_variable(
                        subs, ctx, ctx.first, other,
                    ));
                }

                outcome
            }
            FlatType::RecursiveTagUnion(rec, _, _) => {
                debug_assert!(is_recursion_var(subs, *rec));
                // unify the structure with this recursive tag union
                unify_pool(subs, pool, ctx.first, *structure, ctx.mode)
            }
            FlatType::FunctionOrTagUnion(_, _, _) => {
                // unify the structure with this unrecursive tag union
                let mut outcome = unify_pool(subs, pool, ctx.first, *structure, ctx.mode);

                if outcome.mismatches.is_empty() {
                    outcome.union(fix_tag_union_recursion_variable(
                        subs, ctx, ctx.first, other,
                    ));
                }

                outcome
            }
            // Only tag unions can be recursive; everything else is an error.
            _ => mismatch!(
                "trying to unify {:?} with recursive type var {:?}",
                &flat_type,
                structure
            ),
        },

        Structure(ref other_flat_type) => {
            // Unify the two flat types
            unify_flat_type(subs, pool, ctx, flat_type, other_flat_type)
        }
        Alias(sym, _, real_var, kind) => match kind {
            AliasKind::Structural => {
                // NB: not treating this as a presence constraint seems pivotal! I
                // can't quite figure out why, but it doesn't seem to impact other types.
                unify_pool(subs, pool, ctx.first, *real_var, ctx.mode.as_eq())
            }
            AliasKind::Opaque => {
                mismatch!(
                    "Cannot unify structure {:?} with opaque {:?}",
                    &flat_type,
                    sym
                )
            }
        },
        RangedNumber(other_real_var, other_range_vars) => {
            let outcome = unify_pool(subs, pool, ctx.first, *other_real_var, ctx.mode);
            if outcome.mismatches.is_empty() {
                check_valid_range(subs, pool, ctx.first, *other_range_vars, ctx.mode)
            } else {
                outcome
            }
        }
        Error => merge(subs, ctx, Error),

        FlexAbleVar(_, ability) => {
            // TODO(abilities) support structural types in ability bounds
            mismatch!(
                %not_able, ctx.first, *ability,
                "trying to unify {:?} with FlexAble {:?}",
                &flat_type,
                &other
            )
        }
        RigidAbleVar(_, ability) => {
            mismatch!(
                %not_able, ctx.first, *ability,
                "trying to unify {:?} with RigidAble {:?}",
                &flat_type,
                &other
            )
        }
    }
}

/// Ensures that a non-recursive tag union, when unified with a recursion var to become a recursive
/// tag union, properly contains a recursion variable that recurses on itself.
//
// When might this not be the case? For example, in the code
//
//   Indirect : [ Indirect ConsList ]
//
//   ConsList : [ Nil, Cons Indirect ]
//
//   l : ConsList
//   l = Cons (Indirect (Cons (Indirect Nil)))
//   #   ^^^^^^^^^^^^^^^~~~~~~~~~~~~~~~~~~~~~^ region-a
//   #                  ~~~~~~~~~~~~~~~~~~~~~  region-b
//   l
//
// Suppose `ConsList` has the expanded type `[ Nil, Cons [ Indirect <rec> ] ] as <rec>`.
// After unifying the tag application annotated "region-b" with the recursion variable `<rec>`,
// the tentative total-type of the application annotated "region-a" would be
// `<v> = [ Nil, Cons [ Indirect <v> ] ] as <rec>`. That is, the type of the recursive tag union
// would be inlined at the site "v", rather than passing through the correct recursion variable
// "rec" first.
//
// This is not incorrect from a type perspective, but causes problems later on for e.g. layout
// determination, which expects recursion variables to be placed correctly. Attempting to detect
// this during layout generation does not work so well because it may be that there *are* recursive
// tag unions that should be inlined, and not pass through recursion variables. So instead, try to
// resolve these cases here.
//
// See tests labeled "issue_2810" for more examples.
fn fix_tag_union_recursion_variable(
    subs: &mut Subs,
    ctx: &Context,
    tag_union_promoted_to_recursive: Variable,
    recursion_var: &Content,
) -> Outcome {
    debug_assert!(matches!(
        subs.get_content_without_compacting(tag_union_promoted_to_recursive),
        Structure(FlatType::RecursiveTagUnion(..))
    ));

    let has_recursing_recursive_variable = subs
        .occurs_including_recursion_vars(tag_union_promoted_to_recursive)
        .is_err();

    if !has_recursing_recursive_variable {
        merge(subs, ctx, *recursion_var)
    } else {
        Outcome::default()
    }
}

fn unify_record(
    subs: &mut Subs,
    pool: &mut Pool,
    ctx: &Context,
    fields1: RecordFields,
    ext1: Variable,
    fields2: RecordFields,
    ext2: Variable,
) -> Outcome {
    let (separate, ext1, ext2) = separate_record_fields(subs, fields1, ext1, fields2, ext2);

    let shared_fields = separate.in_both;

    if separate.only_in_1.is_empty() {
        if separate.only_in_2.is_empty() {
            // these variable will be the empty record, but we must still unify them
            let ext_outcome = unify_pool(subs, pool, ext1, ext2, ctx.mode);

            if !ext_outcome.mismatches.is_empty() {
                return ext_outcome;
            }

            let mut field_outcome =
                unify_shared_fields(subs, pool, ctx, shared_fields, OtherFields::None, ext1);

            field_outcome.union(ext_outcome);

            field_outcome
        } else {
            let only_in_2 = RecordFields::insert_into_subs(subs, separate.only_in_2);
            let flat_type = FlatType::Record(only_in_2, ext2);
            let sub_record = fresh(subs, pool, ctx, Structure(flat_type));
            let ext_outcome = unify_pool(subs, pool, ext1, sub_record, ctx.mode);

            if !ext_outcome.mismatches.is_empty() {
                return ext_outcome;
            }

            let mut field_outcome = unify_shared_fields(
                subs,
                pool,
                ctx,
                shared_fields,
                OtherFields::None,
                sub_record,
            );

            field_outcome.union(ext_outcome);

            field_outcome
        }
    } else if separate.only_in_2.is_empty() {
        let only_in_1 = RecordFields::insert_into_subs(subs, separate.only_in_1);
        let flat_type = FlatType::Record(only_in_1, ext1);
        let sub_record = fresh(subs, pool, ctx, Structure(flat_type));
        let ext_outcome = unify_pool(subs, pool, sub_record, ext2, ctx.mode);

        if !ext_outcome.mismatches.is_empty() {
            return ext_outcome;
        }

        let mut field_outcome = unify_shared_fields(
            subs,
            pool,
            ctx,
            shared_fields,
            OtherFields::None,
            sub_record,
        );

        field_outcome.union(ext_outcome);

        field_outcome
    } else {
        let only_in_1 = RecordFields::insert_into_subs(subs, separate.only_in_1);
        let only_in_2 = RecordFields::insert_into_subs(subs, separate.only_in_2);

        let other_fields = OtherFields::Other(only_in_1, only_in_2);

        let ext = fresh(subs, pool, ctx, Content::FlexVar(None));
        let flat_type1 = FlatType::Record(only_in_1, ext);
        let flat_type2 = FlatType::Record(only_in_2, ext);

        let sub1 = fresh(subs, pool, ctx, Structure(flat_type1));
        let sub2 = fresh(subs, pool, ctx, Structure(flat_type2));

        let rec1_outcome = unify_pool(subs, pool, ext1, sub2, ctx.mode);
        if !rec1_outcome.mismatches.is_empty() {
            return rec1_outcome;
        }

        let rec2_outcome = unify_pool(subs, pool, sub1, ext2, ctx.mode);
        if !rec2_outcome.mismatches.is_empty() {
            return rec2_outcome;
        }

        let mut field_outcome =
            unify_shared_fields(subs, pool, ctx, shared_fields, other_fields, ext);

        field_outcome
            .mismatches
            .reserve(rec1_outcome.mismatches.len() + rec2_outcome.mismatches.len());
        field_outcome.union(rec1_outcome);
        field_outcome.union(rec2_outcome);

        field_outcome
    }
}

enum OtherFields {
    None,
    Other(RecordFields, RecordFields),
}

type SharedFields = Vec<(Lowercase, (RecordField<Variable>, RecordField<Variable>))>;

fn unify_shared_fields(
    subs: &mut Subs,
    pool: &mut Pool,
    ctx: &Context,
    shared_fields: SharedFields,
    other_fields: OtherFields,
    ext: Variable,
) -> Outcome {
    let mut matching_fields = Vec::with_capacity(shared_fields.len());
    let num_shared_fields = shared_fields.len();

    for (name, (actual, expected)) in shared_fields {
        let local_outcome = unify_pool(
            subs,
            pool,
            actual.into_inner(),
            expected.into_inner(),
            ctx.mode,
        );

        if local_outcome.mismatches.is_empty() {
            use RecordField::*;

            // Unification of optional fields
            //
            // Demanded does not unify with Optional
            // Unifying Required with Demanded => Demanded
            // Unifying Optional with Required => Required
            // Unifying X with X => X
            let actual = match (actual, expected) {
                (Demanded(_), Optional(_)) | (Optional(_), Demanded(_)) => {
                    // this is an error, but we continue to give better error messages
                    continue;
                }
                (Demanded(val), Required(_))
                | (Required(val), Demanded(_))
                | (Demanded(val), Demanded(_)) => Demanded(val),
                (Required(val), Required(_)) => Required(val),
                (Required(val), Optional(_)) => Required(val),
                (Optional(val), Required(_)) => Required(val),
                (Optional(val), Optional(_)) => Optional(val),
            };

            matching_fields.push((name, actual));
        }
    }

    if num_shared_fields == matching_fields.len() {
        // pull fields in from the ext_var

        let (ext_fields, new_ext_var) = RecordFields::empty().sorted_iterator_and_ext(subs, ext);
        let ext_fields: Vec<_> = ext_fields.into_iter().collect();

        let fields: RecordFields = match other_fields {
            OtherFields::None => {
                if ext_fields.is_empty() {
                    RecordFields::insert_into_subs(subs, matching_fields)
                } else {
                    let all_fields = merge_sorted(matching_fields, ext_fields);
                    RecordFields::insert_into_subs(subs, all_fields)
                }
            }
            OtherFields::Other(other1, other2) => {
                let mut all_fields = merge_sorted(matching_fields, ext_fields);
                all_fields = merge_sorted(
                    all_fields,
                    other1.iter_all().map(|(i1, i2, i3)| {
                        let field_name: Lowercase = subs[i1].clone();
                        let variable = subs[i2];
                        let record_field: RecordField<Variable> = subs[i3].map(|_| variable);

                        (field_name, record_field)
                    }),
                );

                all_fields = merge_sorted(
                    all_fields,
                    other2.iter_all().map(|(i1, i2, i3)| {
                        let field_name: Lowercase = subs[i1].clone();
                        let variable = subs[i2];
                        let record_field: RecordField<Variable> = subs[i3].map(|_| variable);

                        (field_name, record_field)
                    }),
                );

                RecordFields::insert_into_subs(subs, all_fields)
            }
        };

        let flat_type = FlatType::Record(fields, new_ext_var);

        merge(subs, ctx, Structure(flat_type))
    } else {
        mismatch!("in unify_shared_fields")
    }
}

fn separate_record_fields(
    subs: &Subs,
    fields1: RecordFields,
    ext1: Variable,
    fields2: RecordFields,
    ext2: Variable,
) -> (
    Separate<Lowercase, RecordField<Variable>>,
    Variable,
    Variable,
) {
    let (it1, new_ext1) = fields1.sorted_iterator_and_ext(subs, ext1);
    let (it2, new_ext2) = fields2.sorted_iterator_and_ext(subs, ext2);

    let it1 = it1.collect::<Vec<_>>();
    let it2 = it2.collect::<Vec<_>>();

    (separate(it1, it2), new_ext1, new_ext2)
}

#[derive(Debug)]
struct Separate<K, V> {
    only_in_1: Vec<(K, V)>,
    only_in_2: Vec<(K, V)>,
    in_both: Vec<(K, (V, V))>,
}

fn merge_sorted<K, V, I1, I2>(input1: I1, input2: I2) -> Vec<(K, V)>
where
    K: Ord,
    I1: IntoIterator<Item = (K, V)>,
    I2: IntoIterator<Item = (K, V)>,
{
    use std::cmp::Ordering;

    let mut it1 = input1.into_iter().peekable();
    let mut it2 = input2.into_iter().peekable();

    let input1_len = it1.size_hint().0;
    let input2_len = it2.size_hint().0;

    let mut result = Vec::with_capacity(input1_len + input2_len);

    loop {
        let which = match (it1.peek(), it2.peek()) {
            (Some((l, _)), Some((r, _))) => Some(l.cmp(r)),
            (Some(_), None) => Some(Ordering::Less),
            (None, Some(_)) => Some(Ordering::Greater),
            (None, None) => None,
        };

        match which {
            Some(Ordering::Less) => {
                result.push(it1.next().unwrap());
            }
            Some(Ordering::Equal) => {
                let (k, v) = it1.next().unwrap();
                let (_, _) = it2.next().unwrap();
                result.push((k, v));
            }
            Some(Ordering::Greater) => {
                result.push(it2.next().unwrap());
            }
            None => break,
        }
    }

    result
}

fn separate<K, V, I1, I2>(input1: I1, input2: I2) -> Separate<K, V>
where
    K: Ord,
    I1: IntoIterator<Item = (K, V)>,
    I2: IntoIterator<Item = (K, V)>,
{
    use std::cmp::Ordering;

    let mut it1 = input1.into_iter().peekable();
    let mut it2 = input2.into_iter().peekable();

    let input1_len = it1.size_hint().0;
    let input2_len = it2.size_hint().0;

    let max_common = input1_len.min(input2_len);

    let mut result = Separate {
        only_in_1: Vec::with_capacity(input1_len),
        only_in_2: Vec::with_capacity(input2_len),
        in_both: Vec::with_capacity(max_common),
    };

    loop {
        let which = match (it1.peek(), it2.peek()) {
            (Some((l, _)), Some((r, _))) => Some(l.cmp(r)),
            (Some(_), None) => Some(Ordering::Less),
            (None, Some(_)) => Some(Ordering::Greater),
            (None, None) => None,
        };

        match which {
            Some(Ordering::Less) => {
                result.only_in_1.push(it1.next().unwrap());
            }
            Some(Ordering::Equal) => {
                let (k, v1) = it1.next().unwrap();
                let (_, v2) = it2.next().unwrap();
                result.in_both.push((k, (v1, v2)));
            }
            Some(Ordering::Greater) => {
                result.only_in_2.push(it2.next().unwrap());
            }
            None => break,
        }
    }

    result
}

fn separate_union_tags(
    subs: &Subs,
    fields1: UnionTags,
    ext1: Variable,
    fields2: UnionTags,
    ext2: Variable,
) -> (Separate<TagName, VariableSubsSlice>, Variable, Variable) {
    let (it1, new_ext1) = fields1.sorted_slices_iterator_and_ext(subs, ext1);
    let (it2, new_ext2) = fields2.sorted_slices_iterator_and_ext(subs, ext2);

    (separate(it1, it2), new_ext1, new_ext2)
}

#[derive(Debug, Copy, Clone)]
enum Rec {
    None,
    Left(Variable),
    Right(Variable),
    Both(Variable, Variable),
}

#[allow(clippy::too_many_arguments)]
fn unify_tag_union_new(
    subs: &mut Subs,
    pool: &mut Pool,
    ctx: &Context,
    tags1: UnionTags,
    initial_ext1: Variable,
    tags2: UnionTags,
    initial_ext2: Variable,
    recursion_var: Rec,
) -> Outcome {
    let (separate, mut ext1, ext2) =
        separate_union_tags(subs, tags1, initial_ext1, tags2, initial_ext2);

    let shared_tags = separate.in_both;

    if let (true, Content::Structure(FlatType::EmptyTagUnion)) =
        (ctx.mode.is_present(), subs.get(ext1).content)
    {
        if !separate.only_in_2.is_empty() {
            // Create a new extension variable that we'll fill in with the
            // contents of the tag union from our presence contraint.
            //
            // If there's no new (toplevel) tags we need to register for
            // presence, for example in the cases
            //    [A]      += [A]
            //    [A, B]   += [A]
            //    [A M, B] += [A N]
            // then we don't need to create a fresh ext variable, since the
            // tag union is definitely not growing on the top level.
            // Notice that in the last case
            //    [A M, B] += [A N]
            // the nested tag `A` **will** grow, but we don't need to modify
            // the top level extension variable for that!
            let new_ext = fresh(subs, pool, ctx, Content::FlexVar(None));
            let new_union = Structure(FlatType::TagUnion(tags1, new_ext));
            let mut new_desc = ctx.first_desc;
            new_desc.content = new_union;
            subs.set(ctx.first, new_desc);

            ext1 = new_ext;
        }
    }

    if separate.only_in_1.is_empty() {
        if separate.only_in_2.is_empty() {
            let ext_outcome = if ctx.mode.is_eq() {
                unify_pool(subs, pool, ext1, ext2, ctx.mode)
            } else {
                // In a presence context, we don't care about ext2 being equal to ext1
                Outcome::default()
            };

            if !ext_outcome.mismatches.is_empty() {
                return ext_outcome;
            }

            let mut shared_tags_outcome = unify_shared_tags_new(
                subs,
                pool,
                ctx,
                shared_tags,
                OtherTags2::Empty,
                ext1,
                recursion_var,
            );

            shared_tags_outcome.union(ext_outcome);

            shared_tags_outcome
        } else {
            let unique_tags2 = UnionTags::insert_slices_into_subs(subs, separate.only_in_2);
            let flat_type = FlatType::TagUnion(unique_tags2, ext2);
            let sub_record = fresh(subs, pool, ctx, Structure(flat_type));
            let ext_outcome = unify_pool(subs, pool, ext1, sub_record, ctx.mode);

            if !ext_outcome.mismatches.is_empty() {
                return ext_outcome;
            }

            let mut shared_tags_outcome = unify_shared_tags_new(
                subs,
                pool,
                ctx,
                shared_tags,
                OtherTags2::Empty,
                sub_record,
                recursion_var,
            );

            shared_tags_outcome.union(ext_outcome);

            shared_tags_outcome
        }
    } else if separate.only_in_2.is_empty() {
        let unique_tags1 = UnionTags::insert_slices_into_subs(subs, separate.only_in_1);
        let flat_type = FlatType::TagUnion(unique_tags1, ext1);
        let sub_record = fresh(subs, pool, ctx, Structure(flat_type));

        // In a presence context, we don't care about ext2 being equal to tags1
        if ctx.mode.is_eq() {
            let ext_outcome = unify_pool(subs, pool, sub_record, ext2, ctx.mode);

            if !ext_outcome.mismatches.is_empty() {
                return ext_outcome;
            }
        }

        unify_shared_tags_new(
            subs,
            pool,
            ctx,
            shared_tags,
            OtherTags2::Empty,
            sub_record,
            recursion_var,
        )
    } else {
        let other_tags = OtherTags2::Union(separate.only_in_1.clone(), separate.only_in_2.clone());

        let unique_tags1 = UnionTags::insert_slices_into_subs(subs, separate.only_in_1);
        let unique_tags2 = UnionTags::insert_slices_into_subs(subs, separate.only_in_2);

        let ext_content = if ctx.mode.is_present() {
            Content::Structure(FlatType::EmptyTagUnion)
        } else {
            Content::FlexVar(None)
        };
        let ext = fresh(subs, pool, ctx, ext_content);
        let flat_type1 = FlatType::TagUnion(unique_tags1, ext);
        let flat_type2 = FlatType::TagUnion(unique_tags2, ext);

        let sub1 = fresh(subs, pool, ctx, Structure(flat_type1));
        let sub2 = fresh(subs, pool, ctx, Structure(flat_type2));

        // NOTE: for clearer error messages, we rollback unification of the ext vars when either fails
        //
        // This is inspired by
        //
        //
        //      f : [ Red, Green ] -> Bool
        //      f = \_ -> True
        //
        //      f Blue
        //
        //  In this case, we want the mismatch to be between `[ Blue ]a` and `[ Red, Green ]`, but
        //  without rolling back, the mismatch is between `[ Blue, Red, Green ]a` and `[ Red, Green ]`.
        //  TODO is this also required for the other cases?

        let snapshot = subs.snapshot();

        let ext1_outcome = unify_pool(subs, pool, ext1, sub2, ctx.mode);
        if !ext1_outcome.mismatches.is_empty() {
            subs.rollback_to(snapshot);
            return ext1_outcome;
        }

        if ctx.mode.is_eq() {
            let ext2_outcome = unify_pool(subs, pool, sub1, ext2, ctx.mode);
            if !ext2_outcome.mismatches.is_empty() {
                subs.rollback_to(snapshot);
                return ext2_outcome;
            }
        }

        subs.commit_snapshot(snapshot);

        unify_shared_tags_new(subs, pool, ctx, shared_tags, other_tags, ext, recursion_var)
    }
}

#[derive(Debug)]
enum OtherTags2 {
    Empty,
    Union(
        Vec<(TagName, VariableSubsSlice)>,
        Vec<(TagName, VariableSubsSlice)>,
    ),
}

fn maybe_mark_tag_union_recursive(subs: &mut Subs, tag_union_var: Variable) {
    'outer: while let Err((recursive, chain)) = subs.occurs(tag_union_var) {
        let description = subs.get(recursive);
        if let Content::Structure(FlatType::TagUnion(tags, ext_var)) = description.content {
            subs.mark_tag_union_recursive(recursive, tags, ext_var);
        } else {
            // walk the chain till we find a tag union
            for v in &chain[..chain.len() - 1] {
                let description = subs.get(*v);
                if let Content::Structure(FlatType::TagUnion(tags, ext_var)) = description.content {
                    subs.mark_tag_union_recursive(*v, tags, ext_var);
                    continue 'outer;
                }
            }

            panic!("recursive loop does not contain a tag union")
        }
    }
}

fn unify_shared_tags_new(
    subs: &mut Subs,
    pool: &mut Pool,
    ctx: &Context,
    shared_tags: Vec<(TagName, (VariableSubsSlice, VariableSubsSlice))>,
    other_tags: OtherTags2,
    ext: Variable,
    recursion_var: Rec,
) -> Outcome {
    let mut matching_tags = Vec::default();
    let num_shared_tags = shared_tags.len();

    for (name, (actual_vars, expected_vars)) in shared_tags {
        let mut matching_vars = Vec::with_capacity(actual_vars.len());

        let actual_len = actual_vars.len();
        let expected_len = expected_vars.len();

        for (actual_index, expected_index) in actual_vars.into_iter().zip(expected_vars.into_iter())
        {
            let actual = subs[actual_index];
            let expected = subs[expected_index];
            // NOTE the arguments of a tag can be recursive. For instance in the expression
            //
            //  ConsList a : [ Nil, Cons a (ConsList a) ]
            //
            //  Cons 1 (Cons "foo" Nil)
            //
            // We need to not just check the outer layer (inferring ConsList Int)
            // but also the inner layer (finding a type error, as desired)
            //
            // This correction introduces the same issue as in https://github.com/elm/compiler/issues/1964
            // Polymorphic recursion is now a type error.
            //
            // The strategy is to expand the recursive tag union as deeply as the non-recursive one
            // is.
            //
            // > RecursiveTagUnion(rvar, [ Cons a rvar, Nil ], ext)
            //
            // Conceptually becomes
            //
            // > RecursiveTagUnion(rvar, [ Cons a [ Cons a rvar, Nil ], Nil ], ext)
            //
            // and so on until the whole non-recursive tag union can be unified with it.
            //
            // One thing we have to watch out for is that a tag union we're hoping to
            // match a recursive tag union with didn't itself become recursive. If it has,
            // since we're expanding tag unions to equal depths as described above,
            // we'll always pass through this branch. So, we promote tag unions to recursive
            // ones here if it turns out they are that.
            maybe_mark_tag_union_recursive(subs, actual);
            maybe_mark_tag_union_recursive(subs, expected);

            let mut outcome = Outcome::default();

            outcome.union(unify_pool(subs, pool, actual, expected, ctx.mode));

            // clearly, this is very suspicious: these variables have just been unified. And yet,
            // not doing this leads to stack overflows
            if let Rec::Right(_) = recursion_var {
                if outcome.mismatches.is_empty() {
                    matching_vars.push(expected);
                }
            } else if outcome.mismatches.is_empty() {
                matching_vars.push(actual);
            }
        }

        // only do this check after unification so the error message has more info
        if actual_len == expected_len && actual_len == matching_vars.len() {
            matching_tags.push((name, matching_vars));
        }
    }

    if num_shared_tags == matching_tags.len() {
        // pull fields in from the ext_var

        let (ext_fields, new_ext_var) = UnionTags::default().sorted_iterator_and_ext(subs, ext);
        let ext_fields: Vec<_> = ext_fields
            .into_iter()
            .map(|(label, variables)| (label, variables.to_vec()))
            .collect();

        let new_tags: UnionTags = match other_tags {
            OtherTags2::Empty => {
                if ext_fields.is_empty() {
                    UnionTags::insert_into_subs(subs, matching_tags)
                } else {
                    let all_fields = merge_sorted(matching_tags, ext_fields);
                    UnionTags::insert_into_subs(subs, all_fields)
                }
            }
            OtherTags2::Union(other1, other2) => {
                let mut all_fields = merge_sorted(matching_tags, ext_fields);
                all_fields = merge_sorted(
                    all_fields,
                    other1.into_iter().map(|(field_name, subs_slice)| {
                        let vec = subs.get_subs_slice(subs_slice).to_vec();

                        (field_name, vec)
                    }),
                );

                all_fields = merge_sorted(
                    all_fields,
                    other2.into_iter().map(|(field_name, subs_slice)| {
                        let vec = subs.get_subs_slice(subs_slice).to_vec();

                        (field_name, vec)
                    }),
                );

                UnionTags::insert_into_subs(subs, all_fields)
            }
        };

        unify_shared_tags_merge_new(subs, ctx, new_tags, new_ext_var, recursion_var)
    } else {
        mismatch!(
            "Problem with Tag Union\nThere should be {:?} matching tags, but I only got \n{:?}",
            num_shared_tags,
            &matching_tags
        )
    }
}

fn unify_shared_tags_merge_new(
    subs: &mut Subs,
    ctx: &Context,
    new_tags: UnionTags,
    new_ext_var: Variable,
    recursion_var: Rec,
) -> Outcome {
    let flat_type = match recursion_var {
        Rec::None => FlatType::TagUnion(new_tags, new_ext_var),
        Rec::Left(rec) | Rec::Right(rec) | Rec::Both(rec, _) => {
            debug_assert!(is_recursion_var(subs, rec));
            FlatType::RecursiveTagUnion(rec, new_tags, new_ext_var)
        }
    };

    merge(subs, ctx, Structure(flat_type))
}

#[inline(always)]
fn unify_flat_type(
    subs: &mut Subs,
    pool: &mut Pool,
    ctx: &Context,
    left: &FlatType,
    right: &FlatType,
) -> Outcome {
    use roc_types::subs::FlatType::*;

    match (left, right) {
        (EmptyRecord, EmptyRecord) => merge(subs, ctx, Structure(*left)),

        (Record(fields, ext), EmptyRecord) if fields.has_only_optional_fields(subs) => {
            unify_pool(subs, pool, *ext, ctx.second, ctx.mode)
        }

        (EmptyRecord, Record(fields, ext)) if fields.has_only_optional_fields(subs) => {
            unify_pool(subs, pool, ctx.first, *ext, ctx.mode)
        }

        (Record(fields1, ext1), Record(fields2, ext2)) => {
            unify_record(subs, pool, ctx, *fields1, *ext1, *fields2, *ext2)
        }

        (EmptyTagUnion, EmptyTagUnion) => merge(subs, ctx, Structure(*left)),

        (TagUnion(tags, ext), EmptyTagUnion) if tags.is_empty() => {
            unify_pool(subs, pool, *ext, ctx.second, ctx.mode)
        }

        (EmptyTagUnion, TagUnion(tags, ext)) if tags.is_empty() => {
            unify_pool(subs, pool, ctx.first, *ext, ctx.mode)
        }

        (TagUnion(tags1, ext1), TagUnion(tags2, ext2)) => {
            unify_tag_union_new(subs, pool, ctx, *tags1, *ext1, *tags2, *ext2, Rec::None)
        }

        (RecursiveTagUnion(recursion_var, tags1, ext1), TagUnion(tags2, ext2)) => {
            debug_assert!(is_recursion_var(subs, *recursion_var));
            // this never happens in type-correct programs, but may happen if there is a type error

            let rec = Rec::Left(*recursion_var);

            unify_tag_union_new(subs, pool, ctx, *tags1, *ext1, *tags2, *ext2, rec)
        }

        (TagUnion(tags1, ext1), RecursiveTagUnion(recursion_var, tags2, ext2)) => {
            debug_assert!(is_recursion_var(subs, *recursion_var));

            let rec = Rec::Right(*recursion_var);

            unify_tag_union_new(subs, pool, ctx, *tags1, *ext1, *tags2, *ext2, rec)
        }

        (RecursiveTagUnion(rec1, tags1, ext1), RecursiveTagUnion(rec2, tags2, ext2)) => {
            debug_assert!(is_recursion_var(subs, *rec1));
            debug_assert!(is_recursion_var(subs, *rec2));

            let rec = Rec::Both(*rec1, *rec2);
            let mut outcome =
                unify_tag_union_new(subs, pool, ctx, *tags1, *ext1, *tags2, *ext2, rec);
            outcome.union(unify_pool(subs, pool, *rec1, *rec2, ctx.mode));

            outcome
        }

        (Apply(l_symbol, l_args), Apply(r_symbol, r_args)) if l_symbol == r_symbol => {
            let mut outcome = unify_zip_slices(subs, pool, *l_args, *r_args);

            if outcome.mismatches.is_empty() {
                outcome.union(merge(subs, ctx, Structure(Apply(*r_symbol, *r_args))));
            }

            outcome
        }
        (Func(l_args, l_closure, l_ret), Func(r_args, r_closure, r_ret))
            if l_args.len() == r_args.len() =>
        {
            let arg_outcome = unify_zip_slices(subs, pool, *l_args, *r_args);
            let ret_outcome = unify_pool(subs, pool, *l_ret, *r_ret, ctx.mode);
            let closure_outcome = unify_pool(subs, pool, *l_closure, *r_closure, ctx.mode);

            let mut outcome = ret_outcome;

            outcome.union(closure_outcome);
            outcome.union(arg_outcome);

            if outcome.mismatches.is_empty() {
                outcome.union(merge(
                    subs,
                    ctx,
                    Structure(Func(*r_args, *r_closure, *r_ret)),
                ));
            }

            outcome
        }
        (FunctionOrTagUnion(tag_name, tag_symbol, ext), Func(args, closure, ret)) => {
            unify_function_or_tag_union_and_func(
                subs,
                pool,
                ctx,
                tag_name,
                *tag_symbol,
                *ext,
                *args,
                *ret,
                *closure,
                true,
            )
        }
        (Func(args, closure, ret), FunctionOrTagUnion(tag_name, tag_symbol, ext)) => {
            unify_function_or_tag_union_and_func(
                subs,
                pool,
                ctx,
                tag_name,
                *tag_symbol,
                *ext,
                *args,
                *ret,
                *closure,
                false,
            )
        }
        (FunctionOrTagUnion(tag_name_1, _, ext1), FunctionOrTagUnion(tag_name_2, _, ext2)) => {
            let tag_name_1_ref = &subs[*tag_name_1];
            let tag_name_2_ref = &subs[*tag_name_2];

            if tag_name_1_ref == tag_name_2_ref {
                let outcome = unify_pool(subs, pool, *ext1, *ext2, ctx.mode);
                if outcome.mismatches.is_empty() {
                    let content = *subs.get_content_without_compacting(ctx.second);
                    merge(subs, ctx, content)
                } else {
                    outcome
                }
            } else {
                let tags1 = UnionTags::from_tag_name_index(*tag_name_1);
                let tags2 = UnionTags::from_tag_name_index(*tag_name_2);

                unify_tag_union_new(subs, pool, ctx, tags1, *ext1, tags2, *ext2, Rec::None)
            }
        }
        (TagUnion(tags1, ext1), FunctionOrTagUnion(tag_name, _, ext2)) => {
            let tags2 = UnionTags::from_tag_name_index(*tag_name);

            unify_tag_union_new(subs, pool, ctx, *tags1, *ext1, tags2, *ext2, Rec::None)
        }
        (FunctionOrTagUnion(tag_name, _, ext1), TagUnion(tags2, ext2)) => {
            let tags1 = UnionTags::from_tag_name_index(*tag_name);

            unify_tag_union_new(subs, pool, ctx, tags1, *ext1, *tags2, *ext2, Rec::None)
        }

        (RecursiveTagUnion(recursion_var, tags1, ext1), FunctionOrTagUnion(tag_name, _, ext2)) => {
            // this never happens in type-correct programs, but may happen if there is a type error
            debug_assert!(is_recursion_var(subs, *recursion_var));

            let tags2 = UnionTags::from_tag_name_index(*tag_name);
            let rec = Rec::Left(*recursion_var);

            unify_tag_union_new(subs, pool, ctx, *tags1, *ext1, tags2, *ext2, rec)
        }

        (FunctionOrTagUnion(tag_name, _, ext1), RecursiveTagUnion(recursion_var, tags2, ext2)) => {
            debug_assert!(is_recursion_var(subs, *recursion_var));

            let tags1 = UnionTags::from_tag_name_index(*tag_name);
            let rec = Rec::Right(*recursion_var);

            unify_tag_union_new(subs, pool, ctx, tags1, *ext1, *tags2, *ext2, rec)
        }

        (other1, other2) => {
            // any other combination is a mismatch
            mismatch!(
                "Trying to unify two flat types that are incompatible: {:?} ~ {:?}",
                roc_types::subs::SubsFmtFlatType(other1, subs),
                roc_types::subs::SubsFmtFlatType(other2, subs)
            )
        }
    }
}

fn unify_zip_slices(
    subs: &mut Subs,
    pool: &mut Pool,
    left: SubsSlice<Variable>,
    right: SubsSlice<Variable>,
) -> Outcome {
    let mut outcome = Outcome::default();

    let it = left.into_iter().zip(right.into_iter());

    for (l_index, r_index) in it {
        let l_var = subs[l_index];
        let r_var = subs[r_index];

        outcome.union(unify_pool(subs, pool, l_var, r_var, Mode::EQ));
    }

    outcome
}

#[inline(always)]
fn unify_rigid(
    subs: &mut Subs,
    ctx: &Context,
    name: &SubsIndex<Lowercase>,
    opt_able_bound: Option<Symbol>,
    other: &Content,
) -> Outcome {
    match other {
        FlexVar(_) => {
            // If the other is flex, rigid wins!
            merge(subs, ctx, RigidVar(*name))
        }
        FlexAbleVar(_, other_ability) => {
            match opt_able_bound {
                Some(ability) => {
                    if ability == *other_ability {
                        // The ability bounds are the same, so rigid wins!
                        merge(subs, ctx, RigidAbleVar(*name, ability))
                    } else {
                        // Mismatch for now.
                        // TODO check ability hierarchies.
                        mismatch!(
                            %not_able, ctx.second, ability,
                            "RigidAble {:?} with ability {:?} not compatible with ability {:?}",
                            ctx.first,
                            ability,
                            other_ability
                        )
                    }
                }
                None => {
                    // Mismatch - Rigid can unify with FlexAble only when the Rigid has an ability
                    // bound as well, otherwise the user failed to correctly annotate the bound.
                    mismatch!(
                        %not_able, ctx.first, *other_ability,
                        "Rigid {:?} with FlexAble {:?}", ctx.first, other
                    )
                }
            }
        }

        RigidVar(_) | RecursionVar { .. } | Structure(_) | Alias(_, _, _, _) | RangedNumber(..)
            if ctx.mode.contains(Mode::RIGID_AS_FLEX) =>
        {
            // Usually rigids can only unify with flex, but the mode indicates we are treating
            // rigid vars as flex, so admit this.
            match (opt_able_bound, other) {
                (None, other) => merge(subs, ctx, *other),
                (Some(ability), Alias(opaque_name, vars, _real_var, AliasKind::Opaque))
                    if vars.is_empty() =>
                {
                    let mut output = merge(subs, ctx, *other);
                    let must_implement_ability = MustImplementAbility {
                        typ: *opaque_name,
                        ability,
                    };
                    output.must_implement_ability.push(must_implement_ability);
                    output
                }
                (Some(ability), other) => {
                    // For now, only allow opaque types with no type variables to implement abilities.
                    mismatch!(
                        %not_able, ctx.second, ability,
                        "RigidAble {:?} with non-opaque or opaque with type variables {:?}",
                        ctx.first,
                        &other
                    )
                }
            }
        }

        RigidVar(_)
        | RigidAbleVar(..)
        | RecursionVar { .. }
        | Structure(_)
        | Alias(..)
        | RangedNumber(..) => {
            // Type mismatch! Rigid can only unify with flex, even if the
            // rigid names are the same.
            mismatch!("Rigid {:?} with {:?}", ctx.first, &other)
        }

        Error => {
            // Error propagates.
            merge(subs, ctx, Error)
        }
    }
}

#[inline(always)]
fn unify_flex(
    subs: &mut Subs,
    ctx: &Context,
    opt_name: &Option<SubsIndex<Lowercase>>,
    opt_able_bound: Option<Symbol>,
    other: &Content,
) -> Outcome {
    match other {
        FlexVar(None) => {
            // If both are flex, and only left has a name, keep the name around.
            match opt_able_bound {
                Some(ability) => merge(subs, ctx, FlexAbleVar(*opt_name, ability)),
                None => merge(subs, ctx, FlexVar(*opt_name)),
            }
        }

        FlexAbleVar(opt_other_name, other_ability) => {
            // Prefer the right's name when possible.
            let opt_name = (opt_other_name).or(*opt_name);

            match opt_able_bound {
                Some(ability) => {
                    if ability == *other_ability {
                        // The ability bounds are the same! Keep the name around if it exists.
                        merge(subs, ctx, FlexAbleVar(opt_name, ability))
                    } else {
                        // Ability names differ; mismatch for now.
                        // TODO check ability hierarchies.
                        mismatch!(
                            %not_able, ctx.second, ability,
                            "FlexAble {:?} with ability {:?} not compatible with ability {:?}",
                            ctx.first,
                            ability,
                            other_ability
                        )
                    }
                }
                None => {
                    // Right has an ability bound, but left might have the name. Combine them.
                    merge(subs, ctx, FlexAbleVar(opt_name, *other_ability))
                }
            }
        }

        FlexVar(Some(_))
        | RigidVar(_)
        | RigidAbleVar(_, _)
        | RecursionVar { .. }
        | Structure(_)
        | Alias(_, _, _, _)
        | RangedNumber(..) => {
            // TODO special-case boolean here
            // In all other cases, if left is flex, defer to right.
            // (This includes using right's name if both are flex and named.)
            merge(subs, ctx, *other)
        }

        Error => merge(subs, ctx, Error),
    }
}

#[inline(always)]
fn unify_recursion(
    subs: &mut Subs,
    pool: &mut Pool,
    ctx: &Context,
    opt_name: &Option<SubsIndex<Lowercase>>,
    structure: Variable,
    other: &Content,
) -> Outcome {
    match other {
        RecursionVar {
            opt_name: other_opt_name,
            structure: _other_structure,
        } => {
            // NOTE: structure and other_structure may not be unified yet, but will be
            // we should not do that here, it would create an infinite loop!
            let name = (*opt_name).or(*other_opt_name);
            merge(
                subs,
                ctx,
                RecursionVar {
                    opt_name: name,
                    structure,
                },
            )
        }

        Structure(_) => {
            // unify the structure variable with this Structure
            unify_pool(subs, pool, structure, ctx.second, ctx.mode)
        }
        RigidVar(_) => {
            mismatch!("RecursionVar {:?} with rigid {:?}", ctx.first, &other)
        }

        FlexAbleVar(..) | RigidAbleVar(..) => {
            mismatch!("RecursionVar {:?} with able var {:?}", ctx.first, &other)
        }

        FlexVar(_) => merge(
            subs,
            ctx,
            RecursionVar {
                structure,
                opt_name: *opt_name,
            },
        ),

        Alias(opaque, _, _, AliasKind::Opaque) => {
            mismatch!(
                "RecursionVar {:?} cannot be equal to opaque {:?}",
                ctx.first,
                opaque
            )
        }

        Alias(_, _, actual, _) => {
            // look at the type the alias stands for

            unify_pool(subs, pool, ctx.first, *actual, ctx.mode)
        }

        RangedNumber(..) => mismatch!(
            "RecursionVar {:?} with ranged number {:?}",
            ctx.first,
            &other
        ),

        Error => merge(subs, ctx, Error),
    }
}

pub fn merge(subs: &mut Subs, ctx: &Context, content: Content) -> Outcome {
    let rank = ctx.first_desc.rank.min(ctx.second_desc.rank);
    let desc = Descriptor {
        content,
        rank,
        mark: Mark::NONE,
        copy: OptVariable::NONE,
    };

    subs.union(ctx.first, ctx.second, desc);

    Outcome::default()
}

fn register(subs: &mut Subs, desc: Descriptor, pool: &mut Pool) -> Variable {
    let var = subs.fresh(desc);

    pool.push(var);

    var
}

fn fresh(subs: &mut Subs, pool: &mut Pool, ctx: &Context, content: Content) -> Variable {
    register(
        subs,
        Descriptor {
            content,
            rank: ctx.first_desc.rank.min(ctx.second_desc.rank),
            mark: Mark::NONE,
            copy: OptVariable::NONE,
        },
        pool,
    )
}

fn is_recursion_var(subs: &Subs, var: Variable) -> bool {
    matches!(
        subs.get_content_without_compacting(var),
        Content::RecursionVar { .. }
    )
}

#[allow(clippy::too_many_arguments)]
fn unify_function_or_tag_union_and_func(
    subs: &mut Subs,
    pool: &mut Pool,
    ctx: &Context,
    tag_name_index: &SubsIndex<TagName>,
    tag_symbol: Symbol,
    tag_ext: Variable,
    function_arguments: VariableSubsSlice,
    function_return: Variable,
    function_lambda_set: Variable,
    left: bool,
) -> Outcome {
    let tag_name = subs[*tag_name_index].clone();

    let union_tags = UnionTags::insert_slices_into_subs(subs, [(tag_name, function_arguments)]);
    let content = Content::Structure(FlatType::TagUnion(union_tags, tag_ext));

    let new_tag_union_var = fresh(subs, pool, ctx, content);

    let mut outcome = if left {
        unify_pool(subs, pool, new_tag_union_var, function_return, ctx.mode)
    } else {
        unify_pool(subs, pool, function_return, new_tag_union_var, ctx.mode)
    };

    {
        let tag_name = TagName::Closure(tag_symbol);
        let union_tags = UnionTags::tag_without_arguments(subs, tag_name);

        let lambda_set_ext = subs.fresh_unnamed_flex_var();
        let lambda_set_content = Structure(FlatType::TagUnion(union_tags, lambda_set_ext));

        let tag_lambda_set = register(
            subs,
            Descriptor {
                content: lambda_set_content,
                rank: ctx.first_desc.rank.min(ctx.second_desc.rank),
                mark: Mark::NONE,
                copy: OptVariable::NONE,
            },
            pool,
        );

        let closure_outcome = if left {
            unify_pool(subs, pool, tag_lambda_set, function_lambda_set, ctx.mode)
        } else {
            unify_pool(subs, pool, function_lambda_set, tag_lambda_set, ctx.mode)
        };

        outcome.union(closure_outcome);
    }

    if outcome.mismatches.is_empty() {
        let desc = if left {
            subs.get(ctx.second)
        } else {
            subs.get(ctx.first)
        };

        subs.union(ctx.first, ctx.second, desc);
    }

    outcome
}
