use std::sync::Arc;

use anyhow::Result;

use crate::abstraction::{MiddlewareTrait, PipeContextTrait, PipeTrait};

#[derive(Clone)]
pub struct Pipe<C: PipeContextTrait> {
    middlewares: Vec<Arc<dyn MiddlewareTrait<Context = C>>>,
}

pub trait PipeFactory {
    type Context: PipeContextTrait;

    fn create(&self) -> Pipe<Self::Context>;
}

impl<C: PipeContextTrait> Pipe<C> {
    pub fn new() -> Self {
        Self {
            middlewares: Vec::new(),
        }
    }

    pub fn with<M>(mut self, middleware: M) -> Self
    where
        M: MiddlewareTrait<Context = C> + 'static,
    {
        self.middlewares.push(Arc::new(middleware));
        self
    }

    pub fn add_middleware<M>(&mut self, middleware: Arc<M>)
    where
        M: MiddlewareTrait<Context = C> + 'static,
    {
        self.middlewares.push(middleware);
    }

    async fn send_impl(&self, context: C) -> Result<C> {
        let mut context = context;
        for middleware in self.middlewares.iter() {
            context = middleware.send(context).await?;
        }
        Ok(context)
    }
}

#[async_trait::async_trait]
impl<C: PipeContextTrait> PipeTrait for Pipe<C> {
    type Context = C;

    async fn send(&self, context: C) -> Result<C> {
        let cancel_token = context.cancel_token();
        let res = cancel_token
            .run_until_cancelled(self.send_impl(context))
            .await;

        match res {
            Some(res) => res,
            None => anyhow::bail!("Context was cancelled"),
        }
    }
}

impl<C> Default for Pipe<C>
where
    C: PipeContextTrait,
{
    fn default() -> Self {
        Self::new()
    }
}
