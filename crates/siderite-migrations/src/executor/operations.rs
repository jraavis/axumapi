//! Operations helpers for the migration executor.

use super::*;

pub(super) async fn apply_ops_in_order(
    db: &Db,
    kind: BackendKind,
    migration: &Migration,
    state: &ProjectState,
    registry: &crate::registry::MigrationRegistry,
) -> Result<(), MigrationError> {
    let total_sql = schema_editor::statements(kind, state, &migration.operations)?.len();
    // MySQL commits every DDL statement implicitly, so a failure leaves
    // earlier statements applied; a PostgreSQL migration with `atomic: false`
    // runs outside a transaction, where every statement autocommits. Track
    // progress per statement and resume after it on re-run instead of
    // replaying committed statements: an operation can render several (a
    // `CreateModel` with indexes, an `AlterField` changing type *and* an
    // index), and replaying the first ones fails with "already exists".
    let failure = OnFailure::of(kind, migration.atomic);
    let resumable = failure.resumes();
    let mut resume = Progress::default();
    let direction = PROGRESS_APPLY;
    intents::guard(db, migration, registry, direction).await?;
    if resumable {
        reset_progress_direction(db, kind, &migration.id, PROGRESS_APPLY).await?;
        resume =
            load_progress(db, kind, &migration.id, &migration.checksum, PROGRESS_APPLY).await?;
    }
    validate_operation_progress(resume, migration.operations.len())?;
    intents::finish_recorded_callback(db, migration, resume.ops).await?;
    let mut current = state.clone();
    let mut sql_index = 0usize;
    let mut prior_ddl = false;
    // Something already committed by this or an earlier attempt, so a failure
    // leaves partial state: earlier DDL, or earlier DML from a `RunRust` that
    // cannot be rolled back.
    let mut prior_committed = resume.ops > 0 || resume.stmts > 0;
    for (op_no, op) in migration.operations.iter().enumerate() {
        if op_no < resume.ops {
            // Applied by an earlier attempt: advance state without executing.
            if !matches!(op, Operation::RunRust { .. }) {
                let stmts = schema_editor::render(kind, op, &current)?;
                prior_ddl |= stmts.iter().any(|sql| is_implicit_commit(sql));
                sql_index += stmts.len();
            }
            prior_committed = true;
            op.apply_to_state(&mut current)?;
            continue;
        }
        // Statements of this operation an earlier attempt already committed.
        let resume_stmt = if resumable && op_no == resume.ops {
            resume.stmts
        } else {
            0
        };
        if let Operation::RunRust { name, .. } = op {
            validate_statement_progress(resume_stmt, 0)?;
            if resumable && registry.contains(name) {
                intents::begin(db, migration, direction, op_no, Some(name)).await?;
            }
            if let Err(err) = registry.run(name, db).await {
                if resumable && prior_committed {
                    return Err(op_partial(
                        failure,
                        &migration.id,
                        op_no + 1,
                        migration.operations.len(),
                        op.summary(),
                        err,
                    ));
                }
                return Err(err);
            }
            prior_committed = true;
        } else {
            // A partially committed op already passed what the probe guards;
            // re-probing would misreport the half-applied state.
            if resume_stmt == 0 {
                reject_not_null_violations(db, kind, &current, op).await?;
            }
            let preserved = load_sqlite_rebuild_extras(db, kind, &current, op).await?;
            reject_lossy_sqlite_cast(db, kind, &current, op).await?;
            let stmts = schema_editor::render(kind, op, &current)?;
            validate_statement_progress(resume_stmt, stmts.len())?;
            for (stmt_no, sql) in stmts.into_iter().enumerate() {
                sql_index += 1;
                if stmt_no < resume_stmt {
                    // Committed by an earlier attempt: count it, do not replay.
                    prior_ddl |= is_implicit_commit(&sql);
                    prior_committed = true;
                    continue;
                }
                if resumable && matches!(op, Operation::RunSQL { .. }) {
                    intents::begin(db, migration, direction, op_no, None).await?;
                }
                execute_one_sql(
                    db,
                    failure,
                    &migration.id,
                    sql_index,
                    total_sql,
                    failure.prior_committed(prior_committed, prior_ddl),
                    &sql,
                )
                .await?;
                prior_ddl |= is_implicit_commit(&sql);
                prior_committed = true;
                // Not atomic with the statement: MySQL has already committed
                // the DDL, and a PostgreSQL `atomic: false` run has no
                // transaction, so no savepoint can span the two. A crash
                // between them replays the statement on re-run, which fails
                // loudly ("already exists"); see docs/MIGRATIONS.md for manual
                // recovery. That window is inherent to those backends.
                if resumable {
                    save_progress(
                        db,
                        kind,
                        &migration.id,
                        &migration.checksum,
                        PROGRESS_APPLY,
                        Progress {
                            ops: op_no,
                            stmts: stmt_no + 1,
                        },
                    )
                    .await?;
                    if matches!(op, Operation::RunSQL { .. }) {
                        intents::finish(db, migration).await?;
                    }
                }
            }
            if let Some(preserved) = preserved {
                restore_sqlite_rebuild_extras(db, preserved).await?;
            }
            op.apply_to_state(&mut current)?;
        }
        if resumable {
            save_progress(
                db,
                kind,
                &migration.id,
                &migration.checksum,
                PROGRESS_APPLY,
                Progress {
                    ops: op_no + 1,
                    stmts: 0,
                },
            )
            .await?;
            if matches!(op, Operation::RunRust { .. }) {
                intents::finish(db, migration).await?;
            }
        }
    }
    record_history(db, kind, migration).await?;
    if resumable {
        clear_progress(db, kind, &migration.id, PROGRESS_APPLY).await?;
    }
    Ok(())
}

pub(super) async fn unapply_ops_in_order(
    db: &Db,
    kind: BackendKind,
    migration: &Migration,
    state_before: &ProjectState,
    registry: &crate::registry::MigrationRegistry,
) -> Result<(), MigrationError> {
    let operations = &migration.operations;
    let mut snapshots = vec![state_before.clone()];
    let mut current = state_before.clone();
    for op in operations {
        op.apply_to_state(&mut current)?;
        snapshots.push(current.clone());
    }
    current = snapshots
        .last()
        .cloned()
        .unwrap_or_else(|| state_before.clone());
    let mut rev_ops = Vec::new();
    for i in (0..operations.len()).rev() {
        let Some(rev) = operations[i].reverse(&snapshots[i]) else {
            return Err(MigrationError::Irreversible {
                id: migration.id.clone(),
                reason: format!("{} has no reverse", operations[i].summary()),
            });
        };
        rev_ops.push(rev);
    }
    let total_sql = {
        let mut n = 0;
        let mut walk = current.clone();
        for rev in &rev_ops {
            n += schema_editor::render(kind, rev, &walk)?.len();
            rev.apply_to_state(&mut walk)?;
        }
        n
    };
    let mut sql_index = 0usize;
    // See `apply_ops_in_order`: reversing a MySQL migration, or a PostgreSQL
    // one with `atomic: false`, cannot roll its earlier statements back either.
    let failure = OnFailure::of(kind, migration.atomic);
    let resumable = failure.resumes();
    let mut resume = Progress::default();
    let direction = PROGRESS_UNAPPLY;
    intents::guard(db, migration, registry, direction).await?;
    if resumable {
        reset_progress_direction(db, kind, &migration.id, PROGRESS_UNAPPLY).await?;
        resume = load_progress(
            db,
            kind,
            &migration.id,
            &migration.checksum,
            PROGRESS_UNAPPLY,
        )
        .await?;
    }
    validate_operation_progress(resume, rev_ops.len())?;
    intents::finish_recorded_callback(db, migration, resume.ops).await?;
    let mut prior_ddl = false;
    // See `apply_ops_in_order`: earlier DDL or an earlier `RunRust` that
    // committed data already left partial state behind.
    let mut prior_committed = resume.ops > 0 || resume.stmts > 0;
    for (op_no, rev) in rev_ops.into_iter().enumerate() {
        if op_no < resume.ops {
            if !matches!(rev, Operation::RunRust { .. }) {
                let stmts = schema_editor::render(kind, &rev, &current)?;
                prior_ddl |= stmts.iter().any(|sql| is_implicit_commit(sql));
                sql_index += stmts.len();
            }
            prior_committed = true;
            rev.apply_to_state(&mut current)?;
            continue;
        }
        // Statements of this reverse operation an earlier attempt committed.
        let resume_stmt = if resumable && op_no == resume.ops {
            resume.stmts
        } else {
            0
        };
        if let Operation::RunRust { name, .. } = &rev {
            validate_statement_progress(resume_stmt, 0)?;
            if resumable && registry.contains(name) {
                intents::begin(db, migration, direction, op_no, Some(name)).await?;
            }
            if let Err(err) = registry.run(name, db).await {
                if resumable && prior_committed {
                    return Err(op_partial(
                        failure,
                        &migration.id,
                        op_no + 1,
                        operations.len(),
                        rev.summary(),
                        err,
                    ));
                }
                return Err(err);
            }
            prior_committed = true;
        } else {
            if resume_stmt == 0 {
                reject_not_null_violations(db, kind, &current, &rev).await?;
            }
            let preserved = load_sqlite_rebuild_extras(db, kind, &current, &rev).await?;
            reject_lossy_sqlite_cast(db, kind, &current, &rev).await?;
            let stmts = schema_editor::render(kind, &rev, &current)?;
            validate_statement_progress(resume_stmt, stmts.len())?;
            for (stmt_no, sql) in stmts.into_iter().enumerate() {
                sql_index += 1;
                if stmt_no < resume_stmt {
                    prior_ddl |= is_implicit_commit(&sql);
                    prior_committed = true;
                    continue;
                }
                if resumable && matches!(rev, Operation::RunSQL { .. }) {
                    intents::begin(db, migration, direction, op_no, None).await?;
                }
                execute_one_sql(
                    db,
                    failure,
                    &migration.id,
                    sql_index,
                    total_sql,
                    failure.prior_committed(prior_committed, prior_ddl),
                    &sql,
                )
                .await?;
                prior_ddl |= is_implicit_commit(&sql);
                prior_committed = true;
                if resumable {
                    save_progress(
                        db,
                        kind,
                        &migration.id,
                        &migration.checksum,
                        PROGRESS_UNAPPLY,
                        Progress {
                            ops: op_no,
                            stmts: stmt_no + 1,
                        },
                    )
                    .await?;
                    if matches!(rev, Operation::RunSQL { .. }) {
                        intents::finish(db, migration).await?;
                    }
                }
            }
            if let Some(preserved) = preserved {
                restore_sqlite_rebuild_extras(db, preserved).await?;
            }
        }
        rev.apply_to_state(&mut current)?;
        if resumable {
            save_progress(
                db,
                kind,
                &migration.id,
                &migration.checksum,
                PROGRESS_UNAPPLY,
                Progress {
                    ops: op_no + 1,
                    stmts: 0,
                },
            )
            .await?;
            if matches!(rev, Operation::RunRust { .. }) {
                intents::finish(db, migration).await?;
            }
        }
    }
    delete_history(db, kind, &migration.id).await?;
    if resumable {
        clear_progress(db, kind, &migration.id, PROGRESS_UNAPPLY).await?;
    }
    Ok(())
}

fn validate_operation_progress(
    progress: Progress,
    operations: usize,
) -> Result<(), MigrationError> {
    if progress.ops > operations || (progress.ops == operations && progress.stmts != 0) {
        return Err(MigrationError::state(
            "recorded operation progress exceeds the migration",
        ));
    }
    Ok(())
}

fn validate_statement_progress(recorded: usize, statements: usize) -> Result<(), MigrationError> {
    if recorded > statements {
        return Err(MigrationError::state(
            "recorded statement progress exceeds the operation",
        ));
    }
    Ok(())
}
