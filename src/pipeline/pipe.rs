use std::sync::Arc;

use anyhow::Result;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait};

pub struct Pipe<Context: PipeContextTrait> {
    middlewares: Vec<Arc<dyn MiddlewareTrait<Context = Context>>>,
}

impl<Context: PipeContextTrait> Pipe<Context> {
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
        }
    }

    pub fn with(mut self, middleware: impl MiddlewareTrait<Context = Context> + 'static) -> Self {
        self.middlewares.push(Arc::new(middleware));
        self
    }

    pub async fn send(&self, context: &mut Context) -> Result<()> {
        let cancel_token = context.cancel_token();
        let res = cancel_token
            .run_until_cancelled(self.send_impl(context))
            .await;

        match res {
            Some(res) => res,
            None => anyhow::bail!("Context was cancelled"),
        }
    }

    async fn send_impl(&self, context: &mut Context) -> Result<()> {
        for middleware in &self.middlewares {
            middleware.send(context).await?;
        }
        Ok(())
    }
}
