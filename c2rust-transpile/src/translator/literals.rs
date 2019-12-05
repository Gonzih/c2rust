#![deny(missing_docs)]
//! This code is used to generate literal expressions of various kinds.
//! These include integer, floating, array, struct, union, enum literals.

use super::*;
use std::iter;

impl<'c> Translation<'c> {
    /// Generate an integer literal corresponding to the given type, value, and base.
    pub fn mk_int_lit(&self, ty: CQualTypeId, val: u64, base: IntBase) -> Result<P<Expr>, TranslationError> {
        let lit = match base {
            IntBase::Dec => mk().int_lit(val.into(), LitIntType::Unsuffixed),
            IntBase::Hex => mk().float_unsuffixed_lit(format!("0x{:x}", val)),
            IntBase::Oct => mk().float_unsuffixed_lit(format!("0o{:o}", val)),
        };

        let target_ty = self.convert_type(ty.ctype)?;
        Ok(mk().cast_expr(mk().lit_expr(lit), target_ty))
    }

    /// Given an integer value this attempts to either generate the corresponding enum
    /// variant directly, otherwise it transmutes a number to the enum type.
    pub fn enum_for_i64(&self, enum_type_id: CTypeId, value: i64) -> P<Expr> {
        let def_id = match self.ast_context.resolve_type(enum_type_id).kind {
            CTypeKind::Enum(def_id) => def_id,
            _ => panic!("{:?} does not point to an `enum` type"),
        };

        let (variants, underlying_type_id) = match self.ast_context[def_id].kind {
            CDeclKind::Enum {
                ref variants,
                integral_type,
                ..
            } => (variants, integral_type),
            _ => panic!("{:?} does not point to an `enum` declaration"),
        };

        for &variant_id in variants {
            match self.ast_context[variant_id].kind {
                CDeclKind::EnumConstant { value: v, .. } => {
                    if v == ConstIntExpr::I(value) || v == ConstIntExpr::U(value as u64) {
                        let name = self.renamer.borrow().get(&variant_id).unwrap();

                        // Import the enum variant if needed
                        if let Some(cur_file) = *self.cur_file.borrow() {
                            self.add_import(cur_file, variant_id, &name);
                        }
                        return mk().path_expr(vec![name]);
                    }
                }
                _ => panic!("{:?} does not point to an enum variant", variant_id),
            }
        }

        let underlying_type_id =
            underlying_type_id.expect("Attempt to construct value of forward declared enum");
        let value = match self.ast_context.resolve_type(underlying_type_id.ctype).kind {
            CTypeKind::UInt => {
                mk().lit_expr(mk().int_lit((value as u32) as u128, LitIntType::Unsuffixed))
            }
            CTypeKind::ULong => {
                mk().lit_expr(mk().int_lit((value as u64) as u128, LitIntType::Unsuffixed))
            }
            _ => signed_int_expr(value),
        };

        let target_ty = self.convert_type(enum_type_id).unwrap();

        mk().cast_expr(value, target_ty)
    }

    /// Convert a C literal expression to a Rust expression
    pub fn convert_literal(
        &self,
        ctx: ExprContext,
        ty: CQualTypeId,
        kind: &CLiteral,
    ) -> Result<WithStmts<P<Expr>>, TranslationError> {
        match *kind {
            CLiteral::Integer(val, base) => Ok(WithStmts::new_val(self.mk_int_lit(ty, val, base)?)),

            CLiteral::Character(val) => {
                let val = val as u32;
                let expr = match char::from_u32(val) {
                    Some(c) => {
                        let expr = mk().lit_expr(c);
                        let i32_type = mk().path_ty(vec!["i32"]);
                        mk().cast_expr(expr, i32_type)
                    }
                    None => {
                        // Fallback for characters outside of the valid Unicode range
                        if (val as i32) < 0 {
                            mk().unary_expr("-", mk().lit_expr(
                                mk().int_lit((val as i32).abs() as u128, LitIntType::Signed(IntTy::I32))
                            ))
                        } else {
                            mk().lit_expr(
                                mk().int_lit(val as u128, LitIntType::Signed(IntTy::I32))
                            )
                        }
                    }
                };
                Ok(WithStmts::new_val(expr))
            }

            CLiteral::Floating(val, ref c_str) => {
                let mut bytes: Vec<u8> = vec![];
                let str = if c_str.is_empty() {
                    dtoa::write(&mut bytes, val).unwrap();
                    String::from_utf8(bytes).unwrap()
                } else {
                    c_str.to_owned()
                };
                let val = match self.ast_context.resolve_type(ty.ctype).kind {
                    CTypeKind::LongDouble => {
                        self.use_crate(ExternCrate::F128);

                        let fn_path = mk().path_expr(vec!["f128", "f128", "new"]);
                        let args = vec![mk().ident_expr(str)];

                        mk().call_expr(fn_path, args)
                    }
                    CTypeKind::Double => mk().lit_expr(mk().float_lit(str, FloatTy::F64)),
                    CTypeKind::Float => mk().lit_expr(mk().float_lit(str, FloatTy::F32)),
                    ref k => panic!("Unsupported floating point literal type {:?}", k),
                };
                Ok(WithStmts::new_val(val))
            }

            CLiteral::String(ref val, width) => {
                let mut val = val.to_owned();

                let mut expects_uchars = false;
                match self.ast_context.resolve_type(ty.ctype).kind {

                    CTypeKind::ConstantArray(elem_ty, size) => {
                        // Is the element type is unsigned char?
                        if &CTypeKind::UChar == &self.ast_context.resolve_type(elem_ty).kind {
                            expects_uchars = true;
                        }
                        // Match the literal size to the expected size padding with zeros as needed
                        val.resize(size * (width as usize), 0)
                    },

                    // Add zero terminator
                    _ => {
//                        println()
                        for _ in 0..width {
                            val.push(0);
                        }
                    }
                };
                if ctx.is_static {
                    let mut vals: Vec<P<Expr>> = vec![];
                    for c in val {
                        // Emit negative literals if the expected type is not unsigned char. This
                        // provides a fallback for characters outside of the normal ASCII range.
                        // Python 2 doc strings, for example, contain non-ASCII chars (https://git.io/fjAxu).
                        if !expects_uchars && (c as i8) < 0 {
                            // NOTE: the conversion to i32 avoids overflow when calling abs on -128.
                            vals.push(mk().unary_expr("-", mk().lit_expr(
                                mk().int_lit(((c as i8) as i32).abs() as u128, LitIntType::Unsuffixed))
                            ));
                        } else {
                            vals.push(mk().lit_expr(mk().int_lit(c as u128, LitIntType::Unsuffixed)));
                        }
                    }
                    let array = mk().array_expr(vals);
                    Ok(WithStmts::new_val(array))
                } else {
                    let u8_ty = mk().path_ty(vec!["u8"]);
                    let width_lit =
                        mk().lit_expr(mk().int_lit(val.len() as u128, LitIntType::Unsuffixed));
                    let array_ty = mk().array_ty(u8_ty, width_lit);
                    let source_ty = mk().ref_ty(array_ty);
                    let mutbl = if ty.qualifiers.is_const {
                        Mutability::Immutable
                    } else {
                        Mutability::Mutable
                    };
                    let target_ty = mk().set_mutbl(mutbl).ref_ty(self.convert_type(ty.ctype)?);
                    let byte_literal = mk().lit_expr(val);
                    if ctx.is_const { self.use_feature("const_transmute"); }
                    let pointer =
                        transmute_expr(source_ty, target_ty, byte_literal, self.tcfg.emit_no_std);
                    let array = mk().unary_expr(ast::UnOp::Deref, pointer);
                    Ok(WithStmts::new_unsafe_val(array))
                }
            }
        }
    }

    /// Convert an initialization list into an expresion. These initialization lists can be
    /// used as array literals, struct literals, and union literals in code.
    pub fn convert_init_list(
        &self,
        ctx: ExprContext,
        ty: CQualTypeId,
        ids: &[CExprId],
        opt_union_field_id: Option<CFieldId>,
    ) -> Result<WithStmts<P<Expr>>, TranslationError> {
        match self.ast_context.resolve_type(ty.ctype).kind {
            CTypeKind::ConstantArray(ty, n) => {
                // Convert all of the provided initializer values

                // Need to check to see if the next item is a string literal,
                // if it is need to treat it as a declaration, rather than
                // an init list. https://github.com/GaloisInc/C2Rust/issues/40
                let mut is_string = false;

                if ids.len() == 1 {
                    let v = ids.first().unwrap();
                    if let CExprKind::Literal(_, CLiteral::String { .. }) =
                        self.ast_context.index(*v).kind
                    {
                        is_string = true;
                    }
                }

                if is_string {
                    let v = ids.first().unwrap();
                    self.convert_expr(ctx.used(), *v)
                } else {
                    Ok(ids
                        .iter()
                        .map(|id| {
                            self.convert_expr(ctx.used(), *id)?
                                .result_map(|x| {
                                    // Array literals require all of their elements to be
                                    // the correct type; they will not use implicit casts to
                                    // change mut to const. This becomes a problem when an
                                    // array literal is used in a position where there is no
                                    // type information available to force its type to the
                                    // correct const or mut variation. To avoid this issue
                                    // we manually insert the otherwise elided casts in this
                                    // particular context.
                                    if let CExprKind::ImplicitCast(ty, _, CastKind::ConstCast, _, _) =
                                        self.ast_context[*id].kind
                                    {
                                        let t = self.convert_type(ty.ctype)?;
                                        Ok(mk().cast_expr(x, t))
                                    } else {
                                        Ok(x)
                                    }
                                })
                        })
                        .chain(
                            // Pad out the array literal with default values to the desired size
                            iter::repeat(
                                self.implicit_default_expr(ty, ctx.is_static)
                            ).take(n - ids.len())
                        )
                        .collect::<Result<WithStmts<Vec<P<Expr>>>, TranslationError>>()?
                        .map(|vals| {
                            mk().array_expr(vals)
                        }))
                }
            }
            CTypeKind::Struct(struct_id) => {
                let mut literal = self.convert_struct_literal(ctx, struct_id, ids.as_ref());
                if self.ast_context.has_inner_struct_decl(struct_id) {
                    // If the structure is split into an outer/inner,
                    // wrap the inner initializer using the outer structure
                    let outer_name = self.type_converter
                        .borrow()
                        .resolve_decl_name(struct_id)
                        .unwrap();

                    let outer_path = mk().path_expr(vec![outer_name]);
                    literal = literal.map(|lit_ws| {
                        lit_ws.map(|lit| {
                            mk().call_expr(outer_path, vec![lit])
                        })
                    });
                };
                literal
            }
            CTypeKind::Union(union_id) => {
                self.convert_union_literal(ctx, union_id, ids.as_ref(), ty, opt_union_field_id)
            }
            CTypeKind::Pointer(_) => {
                let id = ids.first().unwrap();
                self.convert_expr(ctx.used(), *id)
            }
            CTypeKind::Vector(CQualTypeId { ctype, .. }, len) => {
                self.vector_list_initializer(ctx, ids, ctype, len)
            }
            CTypeKind::Char => {
                let id = ids.first().unwrap();
                self.convert_expr(ctx.used(), *id)
            }
            ref t => Err(format_err!("Init list not implemented for {:?}", t).into()),
        }
    }

    fn convert_union_literal(
        &self,
        ctx: ExprContext,
        union_id: CRecordId,
        ids: &[CExprId],
        _ty: CQualTypeId,
        opt_union_field_id: Option<CFieldId>,
    ) -> Result<WithStmts<P<Expr>>, TranslationError> {
        let union_field_id = opt_union_field_id.expect("union field ID");

        match self.ast_context.index(union_id).kind {
            CDeclKind::Union { .. } => {
                let union_name = self
                    .type_converter
                    .borrow()
                    .resolve_decl_name(union_id)
                    .unwrap();
                match self.ast_context.index(union_field_id).kind {
                    CDeclKind::Field { typ: field_ty, .. } => {
                        let val = if ids.is_empty() {
                            self.implicit_default_expr(field_ty.ctype, ctx.is_static)?
                        } else {
                            self.convert_expr(ctx.used(), ids[0])?
                        };

                        Ok(val.map(|v| {
                            let name = vec![mk().path_segment(union_name)];
                            let field_name = self
                                .type_converter
                                .borrow()
                                .resolve_field_name(Some(union_id), union_field_id)
                                .unwrap();
                            let fields = vec![mk().field(field_name, v)];
                            mk().struct_expr(name, fields)
                        }))
                    }
                    _ => panic!("Union field decl mismatch"),
                }
            }
            _ => panic!("Expected union decl"),
        }
    }
}
