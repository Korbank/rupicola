use rustc_serialize::json::{ToJson, Json};
use config::ParameterDefinition;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum MethodParam {
    /// This is just constant string
    Constant(String),
    /// This is ref to parameter definition
    Variable(Arc<ParameterDefinition>),
    /// Chained MethodParams
    Chained(Vec<MethodParam>),
    /// Capture all params as one-line json string
    Everything,
}
//Helper trait for cleaner implementation
pub trait Unroll {
    fn unroll(&self, params: &Json) -> Result<Option<String>, ()>;
}

impl Unroll for ParameterDefinition {
    fn unroll(&self, params: &Json) -> Result<Option<String>, ()> {
        // get info from params
        // for now variables support only objects
        match params.find(&self.name as &str) {
            Some(ref s) => {
                let conversion_result = self.param_type.convert(s);
                if conversion_result.is_err() {
                    error!("Unable to convert. Value = {:?}; target type = {:?}", s, self);
                }
                conversion_result
            }
            None => {
                if self.optional {
                    // Just wrap default value
                    Ok(self.default.clone())
                } else {
                    error!("Missing required param {:?}", self.name);
                    Err(())
                }
            }
        }
    }
}

impl Unroll for Vec<MethodParam> {
    fn unroll(&self, params: &Json) -> Result<Option<String>, ()> {
        let mut result = String::new();

        for element in self.iter() {
            match element.unroll(params) {
                Ok(Some(ref o)) => result.push_str(o),
                skip @Ok(None) | skip @Err(_) => {
                    if skip.is_ok() {
                        info!("Optional variable {:?} is missing. Skip whole chain", element);
                    }
                    // Return either Ok(None) or Err(..)
                    return skip;
                }
            }
        }
        Ok(Some(result))
    }
}

impl Unroll for MethodParam {
    fn unroll(&self, params: &Json) -> Result<Option<String>, ()> {
        match *self {
            MethodParam::Constant(ref s) => Ok(Some(s.clone())),
            MethodParam::Everything => {
                let json = params.to_json().to_string();
                if json.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(json))
                }
            }
            MethodParam::Variable(ref v) => v.unroll(params),
            MethodParam::Chained(ref c) => c.unroll(params),
        }
    }
}
