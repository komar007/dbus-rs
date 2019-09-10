use std::{fmt, cell};
use std::any::Any;
use crate::{arg, Message, arg::{ReadAll, AppendAll, IterAppend}};
use crate::strings::{Path as PathName, Interface as IfaceName, Member as MemberName, Signature};
use super::crossroads::{Crossroads, PathData, MLookup};
use super::info::{MethodInfo, PropInfo};
use super::MethodErr;

pub struct DebugMethod<H: Handlers>(pub H::Method);
impl<H: Handlers> fmt::Debug for DebugMethod<H> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "...") }
}

pub struct DebugProp<H: Handlers>(pub Option<H::GetProp>, pub Option<H::SetProp>);
impl<H: Handlers> fmt::Debug for DebugProp<H> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "...") }
}

pub trait Handlers {
    type Method;
    type GetProp;
    type SetProp;
    type Iface;
}

/// Parallel tree - Par
#[derive(Debug, Clone, Copy, Default)]
pub struct Par;

impl Par {
    pub fn typed_getprop<I: 'static, T: arg::Arg + arg::Append, G>(getf: G) -> <Par as Handlers>::GetProp
    where G: Fn(&I, &ParInfo) -> Result<T, MethodErr> + Send + Sync + 'static {
        Box::new(move |data, ia, info| {
            let iface: &I = data.downcast_ref().unwrap();
            let t = getf(iface, info)?;
            ia.append(t);
            Ok(())
        })
    }

    pub fn typed_setprop<I: 'static, T: arg::Arg + for <'z> arg::Get<'z>, S>(setf: S) -> <Par as Handlers>::SetProp
    where S: Fn(&I, &ParInfo, T) -> Result<(), MethodErr> + Send + Sync + 'static {
        Box::new(move |data, ii, info| {
            let iface: &I = data.downcast_ref().unwrap();
            let t: T = ii.read()?;
            setf(iface, info, t)
        })
    }
}

#[derive(Debug)]
pub struct ParInfo<'a> {
    lookup: MLookup<'a, Par>,
    message: &'a Message,
}

impl<'a> ParInfo<'a> {
    pub fn msg(&self) -> &Message { self.message }
    pub (super) fn new(msg: &'a Message, lookup: MLookup<'a, Par>) -> Self {
        ParInfo { lookup, message: msg }
    }
    pub fn path_data(&self) -> &PathData<Par> { self.lookup.data }
    pub fn crossroads(&self) -> &Crossroads<Par> { self.lookup.cr }
}

impl Handlers for Par {
    type Method = Box<dyn Fn(&(dyn Any + Send + Sync), &ParInfo) -> Option<Message> + Send + Sync + 'static>;
    type GetProp = Box<dyn Fn(&(dyn Any + Send + Sync), &mut arg::IterAppend, &ParInfo) -> Result<(), MethodErr> + Send + Sync + 'static>;
    type SetProp = Box<dyn Fn(&(dyn Any + Send + Sync), &mut arg::Iter, &ParInfo) -> Result<(), MethodErr> + Send + Sync + 'static>;
    type Iface = Box<dyn Any + 'static + Send + Sync>;
}

impl MethodInfo<'_, Par> {
    pub fn new_par<N, F, T>(name: N, f: F) -> Self where
    F: Fn(&T, &ParInfo) -> Result<Option<Message>, MethodErr> + Send + Sync + 'static,
    N: Into<MemberName<'static>>,
    T: Any + Send + Sync + 'static,
    {
        Self::new(name.into(), Box::new(move |data, info| {
            let x = data.downcast_ref().unwrap();
            f(x, info).unwrap_or_else(|e| { Some(e.to_message(info.message)) })
        }))
    }
}


/// Mutable, non-Send tree
#[derive(Debug, Clone, Copy, Default)]
pub struct Mut;

#[derive(Debug)]
pub struct MutCtx<'a> {
    message: &'a Message,
    send_extra: cell::RefCell<Vec<Message>>,
}

impl<'a> MutCtx<'a> {
    pub fn msg(&self) -> &Message { self.message }
    pub fn send(&self, msg: Message) { self.send_extra.borrow_mut().push(msg); }
    pub (super) fn new(msg: &'a Message) -> Self { MutCtx { message: msg, send_extra: Default::default() } }
}

impl Handlers for Mut {
    type Method = MutMethod;
    type GetProp = Box<dyn FnMut(&mut (dyn Any), &mut arg::IterAppend, &MutCtx) -> Result<(), MethodErr> + 'static>;
    type SetProp = Box<dyn FnMut(&mut (dyn Any), &mut arg::Iter, &MutCtx) -> Result<(), MethodErr> + 'static>;
    type Iface = Box<dyn Any>;
}


pub struct MutMethod(pub (super) MutMethods);

pub (super) enum MutMethods {
    MutIface(Box<dyn FnMut(&mut (dyn Any), &MutCtx) -> Option<Message> + 'static>),
//    Ref(Box<dyn FnMut(&(dyn Any), &Message, &Path) -> Option<Message> + 'static>),
//    MutCr(fn(&mut Crossroads<Mut>, &Message) -> Vec<Message>),
}

/// Internal helper trait
pub trait MakeHandler<T, I, IA, OA> {
    /// Internal helper trait
    fn make(self) -> T;
}

impl<F, I: 'static, IA: ReadAll, OA: AppendAll> MakeHandler<<Par as Handlers>::Method, I, IA, OA> for F
where F: Fn(&I, &ParInfo, IA) -> Result<OA, MethodErr> + Send + Sync + 'static
{
    fn make(self) -> <Par as Handlers>::Method {
        Box::new(move |data, info| {
            let iface: &I = data.downcast_ref().unwrap();
            let r = IA::read(&mut info.msg().iter_init()).map_err(From::from);
            let r = r.and_then(|ia| self(iface, info, ia)); 
            match r {
                Err(e) => Some(e.to_message(info.msg())),
                Ok(r) => {
                    let mut m = info.msg().method_return();
                    OA::append(&r, &mut IterAppend::new(&mut m));
                    Some(m)
                },
            }
        })
    }
}


impl<F, I: 'static, IA: ReadAll, OA: AppendAll> MakeHandler<<Mut as Handlers>::Method, I, IA, OA> for F
where F: FnMut(&mut I, &MutCtx, IA) -> Result<OA, MethodErr> + 'static
{
    fn make(mut self) -> <Mut as Handlers>::Method {
        MutMethod(MutMethods::MutIface(Box::new(move |data, info| {
            let iface: &mut I = data.downcast_mut().unwrap();
            let r = IA::read(&mut info.msg().iter_init()).map_err(From::from);
            let r = r.and_then(|ia| self(iface, info, ia)); 
            match r {
                Err(e) => Some(e.to_message(info.msg())),
                Ok(r) => {
                    let mut m = info.msg().method_return();
                    OA::append(&r, &mut IterAppend::new(&mut m));
                    Some(m)
                },
            }
        })))
    }
}

