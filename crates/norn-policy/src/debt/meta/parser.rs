//! Iterative parser for the closed Rust attribute metadata grammar.

use super::{Meta, MetaForm, MetaId, MetaTree};
use crate::debt::meta_lex::{Token, TokenKind, TokenTag};

pub(super) struct MetaParser<'a> {
    tokens: &'a [Token],
    cursor: usize,
}

struct ListFrame {
    path: Vec<String>,
    start: usize,
    parts: Vec<MetaId>,
}

enum ParsedHead {
    Complete(Meta),
    List(ListFrame),
}

impl<'a> MetaParser<'a> {
    pub(super) const fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, cursor: 0 }
    }

    pub(super) fn parse_meta(&mut self) -> Result<MetaTree, usize> {
        let root_frame = match self.parse_head()? {
            ParsedHead::Complete(root) => {
                return Ok(MetaTree {
                    nodes: vec![root],
                    root: 0,
                });
            }
            ParsedHead::List(frame) => frame,
        };

        let mut nodes = Vec::new();
        let mut frames = vec![root_frame];
        let mut expects_item = true;
        loop {
            if expects_item {
                if let Some(close) = self.consume(TokenTag::Close) {
                    if let Some(root) = close_frame(&mut frames, &mut nodes, close.end)? {
                        return Ok(MetaTree { nodes, root });
                    }
                    expects_item = false;
                    continue;
                }
                match self.parse_head()? {
                    ParsedHead::Complete(meta) => {
                        let meta_id = push_meta(&mut nodes, meta);
                        let Some(frame) = frames.last_mut() else {
                            return Err(self.offset());
                        };
                        frame.parts.push(meta_id);
                        expects_item = false;
                    }
                    ParsedHead::List(frame) => frames.push(frame),
                }
                continue;
            }

            if self.consume(TokenTag::Comma).is_some() {
                expects_item = true;
            } else if let Some(close) = self.consume(TokenTag::Close) {
                if let Some(root) = close_frame(&mut frames, &mut nodes, close.end)? {
                    return Ok(MetaTree { nodes, root });
                }
            } else {
                return Err(self.offset());
            }
        }
    }

    fn parse_head(&mut self) -> Result<ParsedHead, usize> {
        let Some(first) = self.next() else {
            return Err(self.offset());
        };
        let TokenKind::Identifier(first_name) = &first.kind else {
            return Err(first.start);
        };
        let mut path = vec![first_name.clone()];
        let mut end = first.end;
        while self.consume(TokenTag::PathSeparator).is_some() {
            let segment = self.next().ok_or(end)?;
            let TokenKind::Identifier(name) = &segment.kind else {
                return Err(segment.start);
            };
            path.push(name.clone());
            end = segment.end;
        }
        if self.consume(TokenTag::Open).is_some() {
            return Ok(ParsedHead::List(ListFrame {
                path,
                start: first.start,
                parts: Vec::new(),
            }));
        }
        let form = if self.consume(TokenTag::Equal).is_some() {
            let value = self.next().ok_or(end)?;
            end = value.end;
            match &value.kind {
                TokenKind::Identifier(value) => MetaForm::Equals(value.as_bytes().to_vec()),
                TokenKind::Literal(value) => MetaForm::Equals(value.clone()),
                _ => return Err(value.start),
            }
        } else {
            MetaForm::Word
        };
        Ok(ParsedHead::Complete(Meta {
            path,
            start: first.start,
            end,
            form,
        }))
    }

    fn consume(&mut self, tag: TokenTag) -> Option<&'a Token> {
        let token = self.tokens.get(self.cursor)?;
        if !tag.matches(&token.kind) {
            return None;
        }
        self.cursor += 1;
        Some(token)
    }

    fn next(&mut self) -> Option<&'a Token> {
        let token = self.tokens.get(self.cursor)?;
        self.cursor += 1;
        Some(token)
    }

    pub(super) const fn is_finished(&self) -> bool {
        self.cursor == self.tokens.len()
    }

    pub(super) fn offset(&self) -> usize {
        self.tokens.get(self.cursor).map_or_else(
            || self.tokens.last().map_or(0, |token| token.end),
            |token| token.start,
        )
    }
}

fn push_meta(nodes: &mut Vec<Meta>, meta: Meta) -> MetaId {
    let meta_id = nodes.len();
    nodes.push(meta);
    meta_id
}

fn close_frame(
    frames: &mut Vec<ListFrame>,
    nodes: &mut Vec<Meta>,
    end: usize,
) -> Result<Option<MetaId>, usize> {
    let Some(frame) = frames.pop() else {
        return Err(end);
    };
    let meta_id = push_meta(
        nodes,
        Meta {
            path: frame.path,
            start: frame.start,
            end,
            form: MetaForm::List(frame.parts),
        },
    );
    let Some(parent) = frames.last_mut() else {
        return Ok(Some(meta_id));
    };
    parent.parts.push(meta_id);
    Ok(None)
}
