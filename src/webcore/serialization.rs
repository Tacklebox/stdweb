use std::mem;
use std::slice;
use std::i32;
use std::collections::{BTreeMap, HashMap};
use std::marker::PhantomData;
use std::cell::{Cell, UnsafeCell};
use std::hash::Hash;

use webcore::ffi;
use webcore::callfn::{CallOnce, CallMut};
use webcore::newtype::Newtype;
use webcore::try_from::{TryFrom, TryInto};
use webcore::number::Number;
use webcore::type_name::type_name;
use webcore::symbol::Symbol;
use webcore::unsafe_typed_array::UnsafeTypedArray;
use webcore::once::Once;

use webcore::value::{
    Null,
    Undefined,
    Reference,
    Value
};

#[repr(u8)]
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Tag {
    Undefined = 0,
    Null = 1,
    I32 = 2,
    F64 = 3,
    Str = 4,
    False = 5,
    True = 6,
    Array = 7,
    Object = 8,
    Reference = 9,
    Function = 10,
    FunctionOnce = 13,
    UnsafeTypedArray = 14,
    Symbol = 15
}

impl Default for Tag {
    #[inline]
    fn default() -> Self {
        Tag::Undefined
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub struct PreallocatedArena {
    saved: UnsafeCell< Vec< Value > >,
    buffer: UnsafeCell< Vec< u8 > >,
    index: Cell< usize >
}

impl PreallocatedArena {
    #[doc(hidden)]
    #[inline]
    pub fn new( memory_required: usize ) -> Self {
        let mut buffer = Vec::new();
        buffer.reserve( memory_required );
        unsafe {
            buffer.set_len( memory_required );
        }

        PreallocatedArena {
            saved: UnsafeCell::new( Vec::new() ),
            buffer: UnsafeCell::new( buffer ),
            index: Cell::new( 0 )
        }
    }

    #[doc(hidden)]
    #[inline]
    pub fn reserve< T >( &self, count: usize ) -> &mut [T] {
        let bytes = mem::size_of::< T >() * count;
        let slice = unsafe {
            let buffer = &mut *self.buffer.get();
            debug_assert!( self.index.get() + bytes <= buffer.len() );

            slice::from_raw_parts_mut(
                buffer.as_mut_ptr().offset( self.index.get() as isize ) as *mut T,
                count
            )
        };

        self.index.set( self.index.get() + bytes );
        slice
    }

    #[doc(hidden)]
    #[inline]
    pub fn save( &self, value: Value ) -> &Value {
        unsafe {
            let saved = &mut *self.saved.get();
            saved.push( value );
            &*(saved.last().unwrap() as *const Value)
        }
    }

    #[doc(hidden)]
    #[inline]
    pub fn assert_no_free_space_left( &self ) {
        debug_assert!( self.index.get() == unsafe { &*self.buffer.get() }.len() );
    }
}

#[doc(hidden)]
pub trait JsSerializeOwned: Sized {
    fn into_js_owned< 'a >( value: &'a mut Option< Self >, arena: &'a PreallocatedArena ) -> SerializedValue< 'a >;
    fn memory_required_owned( &self ) -> usize;
}

/// A trait for types which can be serialized through the `js!` macro.
///
/// Do **not** try to implement this trait yourself! It's only meant
/// to be used inside generic code for specifying trait bounds.
pub trait JsSerialize {
    #[doc(hidden)]
    fn _into_js< 'a >( &'a self, arena: &'a PreallocatedArena ) -> SerializedValue< 'a >;

    #[doc(hidden)]
    fn _memory_required( &self ) -> usize;
}

// This is a generic structure for serializing every JavaScript value.
#[doc(hidden)]
#[repr(C)]
#[derive(Default, Debug)]
pub struct SerializedValue< 'a > {
    data_1: u64,
    data_2: u32,
    tag: Tag,
    phantom: PhantomData< &'a () >
}

#[test]
fn test_serialized_value_size() {
    assert_eq!( mem::size_of::< SerializedValue< 'static > >(), 16 );
}

#[repr(C)]
#[derive(Debug)]
struct SerializedUntaggedUndefined;

#[repr(C)]
#[derive(Debug)]
struct SerializedUntaggedNull;

#[repr(C)]
#[derive(Debug)]
struct SerializedUntaggedI32 {
    value: i32
}

#[repr(C)]
#[derive(Debug)]
struct SerializedUntaggedF64 {
    value: f64
}

#[repr(C)]
#[derive(Debug)]
struct SerializedUntaggedTrue {}

#[repr(C)]
#[derive(Debug)]
struct SerializedUntaggedFalse {}

#[repr(C)]
#[derive(Clone, Debug)]
struct SerializedUntaggedString {
    pointer: u32,
    length: u32
}

#[repr(C)]
#[derive(Clone, Debug)]
struct SerializedUntaggedArray {
    pointer: u32,
    length: u32
}

#[repr(C)]
#[derive(Debug)]
struct SerializedUntaggedObject {
    value_pointer: u32,
    length: u32,
    key_pointer: u32
}

#[repr(C)]
#[derive(Debug)]
struct SerializedUntaggedSymbol {
    id: i32
}

#[repr(C)]
#[derive(Debug)]
struct SerializedUntaggedReference {
    refid: i32
}

#[repr(C)]
#[derive(Debug)]
struct SerializedUntaggedFunction {
    adapter_pointer: u32,
    pointer: u32,
    deallocator_pointer: u32
}

#[repr(C)]
#[derive(Debug)]
struct SerializedUntaggedFunctionOnce {
    adapter_pointer: u32,
    pointer: u32,
    deallocator_pointer: u32
}

#[repr(C)]
#[derive(Debug)]
struct SerializedUntaggedUnsafeTypedArray {
    pointer: u32,
    length: u32,
    kind: u32
}

impl SerializedUntaggedString {
    #[inline]
    fn deserialize( &self ) -> String {
        let pointer = self.pointer as *mut u8;
        let length = self.length as usize;

        if length == 0 {
            return String::new();
        }

        unsafe {
            let vector = Vec::from_raw_parts( pointer, length, length + 1 );
            String::from_utf8_unchecked( vector )
        }
    }
}

impl SerializedUntaggedArray {
    #[inline]
    fn deserialize( &self ) -> Vec< Value > {
        let pointer = self.pointer as *const SerializedValue;
        let length = self.length as usize;
        let slice = unsafe {
            slice::from_raw_parts( pointer, length )
        };

        let vector = slice.iter().map( |value| value.deserialize() ).collect();
        unsafe {
            ffi::dealloc( pointer as *mut u8, length * mem::size_of::< SerializedValue >() );
        }

        vector
    }
}

pub struct ObjectDeserializer< 'a > {
    key_slice: &'a [SerializedUntaggedString],
    value_slice: &'a [SerializedValue< 'a >],
    index: usize
}

impl< 'a > Iterator for ObjectDeserializer< 'a > {
    type Item = (String, Value);
    fn next( &mut self ) -> Option< Self::Item > {
        if self.index >= self.key_slice.len() {
            None
        } else {
            let key = self.key_slice[ self.index ].deserialize();
            let value = self.value_slice[ self.index ].deserialize();
            self.index += 1;
            Some( (key, value) )
        }
    }

    #[inline]
    fn size_hint( &self ) -> (usize, Option< usize >) {
        let remaining = self.key_slice.len() - self.index;
        (remaining, Some( remaining ))
    }
}

impl< 'a > ExactSizeIterator for ObjectDeserializer< 'a > {}

pub fn deserialize_object< R, F: FnOnce( &mut ObjectDeserializer ) -> R >( reference: &Reference, callback: F ) -> R {
    let mut result: SerializedValue = Default::default();
    __js_raw_asm!( "\
        var object = Module.STDWEB_PRIVATE.acquire_js_reference( $0 );\
        Module.STDWEB_PRIVATE.serialize_object( $1, object );",
        reference.as_raw(),
        &mut result as *mut _
    );

    assert_eq!( result.tag, Tag::Object );
    let result = result.as_object();

    let length = result.length as usize;
    let key_pointer = result.key_pointer as *const SerializedUntaggedString;
    let value_pointer = result.value_pointer as *const SerializedValue;

    let key_slice = unsafe { slice::from_raw_parts( key_pointer, length ) };
    let value_slice = unsafe { slice::from_raw_parts( value_pointer, length ) };

    let mut iter = ObjectDeserializer {
        key_slice,
        value_slice,
        index: 0
    };

    let output = callback( &mut iter );

    // TODO: Panic-safety.
    unsafe {
        ffi::dealloc( key_pointer as *mut u8, length * mem::size_of::< SerializedUntaggedString >() );
        ffi::dealloc( value_pointer as *mut u8, length * mem::size_of::< SerializedValue >() );
    }

    output
}

pub struct ArrayDeserializer< 'a > {
    slice: &'a [SerializedValue< 'a >],
    index: usize
}

impl< 'a > Iterator for ArrayDeserializer< 'a > {
    type Item = Value;
    fn next( &mut self ) -> Option< Self::Item > {
        if self.index >= self.slice.len() {
            None
        } else {
            let value = self.slice[ self.index ].deserialize();
            self.index += 1;
            Some( value )
        }
    }

    #[inline]
    fn size_hint( &self ) -> (usize, Option< usize >) {
        let remaining = self.slice.len() - self.index;
        (remaining, Some( remaining ))
    }
}

impl< 'a > ExactSizeIterator for ArrayDeserializer< 'a > {}

pub fn deserialize_array< R, F: FnOnce( &mut ArrayDeserializer ) -> R >( reference: &Reference, callback: F ) -> R {
    let mut result: SerializedValue = Default::default();
    __js_raw_asm!( "\
        var array = Module.STDWEB_PRIVATE.acquire_js_reference( $0 );\
        Module.STDWEB_PRIVATE.serialize_array( $1, array );",
        reference.as_raw(),
        &mut result as *mut _
    );

    assert_eq!( result.tag, Tag::Array );
    let result = result.as_array();

    let length = result.length as usize;
    let pointer = result.pointer as *const SerializedValue;

    let slice = unsafe { slice::from_raw_parts( pointer, length ) };
    let mut iter = ArrayDeserializer {
        slice,
        index: 0
    };

    let output = callback( &mut iter );

    // TODO: Panic-safety.
    unsafe {
        ffi::dealloc( pointer as *mut u8, length * mem::size_of::< SerializedValue >() );
    }

    output
}

impl SerializedUntaggedSymbol {
    #[inline]
    fn deserialize( &self ) -> Symbol {
        Symbol( self.id )
    }
}

impl SerializedUntaggedReference {
    #[inline]
    fn deserialize( &self ) -> Reference {
        unsafe { Reference::from_raw_unchecked_noref( self.refid ) }
    }
}

macro_rules! untagged_boilerplate {
    ($tests_namespace:ident, $reader_name:ident, $tag:expr, $untagged_type:ident) => {
        impl< 'a > SerializedValue< 'a > {
            #[allow(dead_code)]
            #[inline]
            fn $reader_name( &self ) -> &$untagged_type {
                debug_assert_eq!( self.tag, $tag );
                unsafe {
                    &*(self as *const _ as *const $untagged_type)
                }
            }
        }

        impl< 'a > From< $untagged_type > for SerializedValue< 'a > {
            #[inline]
            fn from( untagged: $untagged_type ) -> Self {
                unsafe {
                    let mut value: SerializedValue = mem::uninitialized();
                    *(&mut value as *mut SerializedValue as *mut $untagged_type) = untagged;
                    value.tag = $tag;
                    value
                }
            }
        }

        #[cfg(test)]
        mod $tests_namespace {
            use super::*;

            #[test]
            fn does_not_overlap_with_the_tag() {
                let size = mem::size_of::< $untagged_type >();
                let tag_offset = unsafe { &(&*(0 as *const SerializedValue< 'static >)).tag as *const _ as usize };
                assert!( size <= tag_offset );
            }
        }
    }
}

untagged_boilerplate!( test_undefined, as_undefined, Tag::Undefined, SerializedUntaggedUndefined );
untagged_boilerplate!( test_null, as_null, Tag::Null, SerializedUntaggedNull );
untagged_boilerplate!( test_i32, as_i32, Tag::I32, SerializedUntaggedI32 );
untagged_boilerplate!( test_f64, as_f64, Tag::F64, SerializedUntaggedF64 );
untagged_boilerplate!( test_true, as_true, Tag::True, SerializedUntaggedTrue );
untagged_boilerplate!( test_false, as_false, Tag::False, SerializedUntaggedFalse );
untagged_boilerplate!( test_object, as_object, Tag::Object, SerializedUntaggedObject );
untagged_boilerplate!( test_string, as_string, Tag::Str, SerializedUntaggedString );
untagged_boilerplate!( test_array, as_array, Tag::Array, SerializedUntaggedArray );
untagged_boilerplate!( test_symbol, as_symbol, Tag::Symbol, SerializedUntaggedSymbol );
untagged_boilerplate!( test_reference, as_reference, Tag::Reference, SerializedUntaggedReference );
untagged_boilerplate!( test_function, as_function, Tag::Function, SerializedUntaggedFunction );
untagged_boilerplate!( test_function_once, as_function_once, Tag::FunctionOnce, SerializedUntaggedFunctionOnce );
untagged_boilerplate!( test_unsafe_typed_array, as_unsafe_typed_array, Tag::UnsafeTypedArray, SerializedUntaggedUnsafeTypedArray );

impl< 'a > SerializedValue< 'a > {
    #[doc(hidden)]
    #[inline]
    pub fn deserialize( &self ) -> Value {
        match self.tag {
            Tag::Undefined => Value::Undefined,
            Tag::Null => Value::Null,
            Tag::I32 => self.as_i32().value.into(),
            Tag::F64 => self.as_f64().value.into(),
            Tag::Str => Value::String( self.as_string().deserialize() ),
            Tag::False => Value::Bool( false ),
            Tag::True => Value::Bool( true ),
            Tag::Reference => self.as_reference().deserialize().into(),
            Tag::Symbol => self.as_symbol().deserialize().into(),
            Tag::Function |
            Tag::FunctionOnce |
            Tag::Object |
            Tag::Array |
            Tag::UnsafeTypedArray => unreachable!()
        }
    }
}

impl JsSerialize for () {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedUndefined.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        0
    }
}

__js_serializable_boilerplate!( () );

impl JsSerialize for Undefined {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedUndefined.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        0
    }
}

__js_serializable_boilerplate!( Undefined );

impl JsSerialize for Null {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedNull.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        0
    }
}

__js_serializable_boilerplate!( Null );

impl JsSerialize for Symbol {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedSymbol {
            id: self.0
        }.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        0
    }
}

__js_serializable_boilerplate!( Symbol );

impl JsSerialize for Reference {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedReference {
            refid: self.as_raw()
        }.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        0
    }
}

__js_serializable_boilerplate!( Reference );

impl JsSerialize for bool {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        if *self {
            SerializedUntaggedTrue {}.into()
        } else {
            SerializedUntaggedFalse {}.into()
        }
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        0
    }
}

__js_serializable_boilerplate!( bool );

impl JsSerialize for str {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedString {
            pointer: self.as_ptr() as u32,
            length: self.len() as u32
        }.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        0
    }
}

__js_serializable_boilerplate!( impl< 'a > for &'a str );

impl JsSerialize for String {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, arena: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        self.as_str()._into_js( arena )
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        self.as_str()._memory_required()
    }
}

__js_serializable_boilerplate!( String );

impl JsSerialize for i8 {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedI32 {
            value: *self as i32
        }.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        (*self as i32)._memory_required()
    }
}

__js_serializable_boilerplate!( i8 );

impl JsSerialize for i16 {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedI32 {
            value: *self as i32
        }.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        (*self as i32)._memory_required()
    }
}

__js_serializable_boilerplate!( i16 );

impl JsSerialize for i32 {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedI32 {
            value: *self
        }.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        0
    }
}

__js_serializable_boilerplate!( i32 );

impl JsSerialize for u8 {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedI32 {
            value: *self as i32
        }.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        (*self as i32)._memory_required()
    }
}

__js_serializable_boilerplate!( u8 );

impl JsSerialize for u16 {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedI32 {
            value: *self as i32
        }.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        (*self as i32)._memory_required()
    }
}

__js_serializable_boilerplate!( u16 );

impl JsSerialize for u32 {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedF64 {
            value: *self as f64
        }.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        (*self as f64)._memory_required()
    }
}

__js_serializable_boilerplate!( u32 );

impl JsSerialize for f32 {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedF64 {
            value: *self as f64
        }.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        (*self as f64)._memory_required()
    }
}

__js_serializable_boilerplate!( f32 );

impl JsSerialize for f64 {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        SerializedUntaggedF64 {
            value: *self
        }.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        0
    }
}

__js_serializable_boilerplate!( f64 );

impl JsSerialize for Number {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, arena: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        use webcore::number::{Storage, get_storage};
        match *get_storage( self ) {
            Storage::I32( ref value ) => value._into_js( arena ),
            Storage::F64( ref value ) => value._into_js( arena )
        }
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        0
    }
}

__js_serializable_boilerplate!( Number );

impl< T: JsSerialize > JsSerialize for Option< T > {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, arena: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        if let Some( value ) = self.as_ref() {
            value._into_js( arena )
        } else {
            SerializedUntaggedNull.into()
        }
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        if let Some( value ) = self.as_ref() {
            value._memory_required()
        } else {
            0
        }
    }
}

__js_serializable_boilerplate!( impl< T > for Option< T > where T: JsSerialize );

impl< T: JsSerialize > JsSerialize for [T] {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, arena: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        let output = arena.reserve( self.len() );
        for (value, output_value) in self.iter().zip( output.iter_mut() ) {
            *output_value = value._into_js( arena );
        }

        SerializedUntaggedArray {
            pointer: output.as_ptr() as u32,
            length: output.len() as u32
        }.into()
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        mem::size_of::< SerializedValue >() * self.len() +
        self.iter().fold( 0, |sum, value| sum + value._memory_required() )
    }
}

__js_serializable_boilerplate!( impl< 'a, T > for &'a [T] where T: JsSerialize );

impl< T: JsSerialize > JsSerialize for Vec< T > {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, arena: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        self.as_slice()._into_js( arena )
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        self.as_slice()._memory_required()
    }
}

__js_serializable_boilerplate!( impl< T > for Vec< T > where T: JsSerialize );

fn object_into_js< 'a, K: AsRef< str >, V: 'a + JsSerialize, I: Iterator< Item = (K, &'a V) > + ExactSizeIterator >( iter: I, arena: &'a PreallocatedArena ) -> SerializedValue< 'a > {
    let keys = arena.reserve( iter.len() );
    let values = arena.reserve( iter.len() );
    for (((key, value), output_key), output_value) in iter.zip( keys.iter_mut() ).zip( values.iter_mut() ) {
        *output_key = key.as_ref()._into_js( arena ).as_string().clone();
        *output_value = value._into_js( arena );
    }

    SerializedUntaggedObject {
        key_pointer: keys.as_ptr() as u32,
        value_pointer: values.as_ptr() as u32,
        length: keys.len() as u32
    }.into()
}

fn object_memory_required< K: AsRef< str >, V: JsSerialize, I: Iterator< Item = (K, V) > + ExactSizeIterator >( iter: I ) -> usize {
    mem::size_of::< SerializedValue >() * iter.len() +
    mem::size_of::< SerializedUntaggedString >() * iter.len() +
    iter.fold( 0, |sum, (key, value)| sum + key.as_ref()._memory_required() + value._memory_required() )
}

impl< K: AsRef< str >, V: JsSerialize > JsSerialize for BTreeMap< K, V > {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, arena: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        object_into_js( self.iter(), arena )
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        object_memory_required( self.iter() )
    }
}

__js_serializable_boilerplate!( impl< K, V > for BTreeMap< K, V > where K: AsRef< str >, V: JsSerialize );

impl< K: AsRef< str > + Eq + Hash, V: JsSerialize > JsSerialize for HashMap< K, V > {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'a >( &'a self, arena: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        object_into_js( self.iter(), arena )
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        object_memory_required( self.iter() )
    }
}

__js_serializable_boilerplate!( impl< K, V > for HashMap< K, V > where K: AsRef< str > + Eq + Hash, V: JsSerialize );

impl JsSerialize for Value {
    #[doc(hidden)]
    fn _into_js< 'a >( &'a self, arena: &'a PreallocatedArena ) -> SerializedValue< 'a > {
        match *self {
            Value::Undefined => SerializedUntaggedUndefined.into(),
            Value::Null => SerializedUntaggedNull.into(),
            Value::Bool( ref value ) => value._into_js( arena ),
            Value::Number( ref value ) => value._into_js( arena ),
            Value::Symbol( ref value ) => value._into_js( arena ),
            Value::String( ref value ) => value._into_js( arena ),
            Value::Reference( ref value ) => value._into_js( arena )
        }
    }

    #[doc(hidden)]
    fn _memory_required( &self ) -> usize {
        match *self {
            Value::Undefined => Undefined._memory_required(),
            Value::Null => Null._memory_required(),
            Value::Bool( value ) => value._memory_required(),
            Value::Number( value ) => value._memory_required(),
            Value::Symbol( ref value ) => value._memory_required(),
            Value::String( ref value ) => value._memory_required(),
            Value::Reference( ref value ) => value._memory_required()
        }
    }
}

__js_serializable_boilerplate!( Value );

macro_rules! impl_for_unsafe_typed_array {
    ($ty:ty, $kind:expr) => {
        impl< 'r > JsSerialize for UnsafeTypedArray< 'r, $ty > {
            #[doc(hidden)]
            #[inline]
            fn _into_js< 'a >( &'a self, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
                SerializedUntaggedUnsafeTypedArray {
                    pointer: self.0.as_ptr() as u32 / mem::size_of::< $ty >() as u32,
                    length: self.0.len() as u32,
                    kind: $kind
                }.into()
            }

            #[doc(hidden)]
            #[inline]
            fn _memory_required( &self ) -> usize {
                0
            }
        }

        __js_serializable_boilerplate!( impl< 'a > for UnsafeTypedArray< 'a, $ty > );
    }
}

impl_for_unsafe_typed_array!( u8, 0 );
impl_for_unsafe_typed_array!( i8, 1 );
impl_for_unsafe_typed_array!( u16, 2 );
impl_for_unsafe_typed_array!( i16, 3 );
impl_for_unsafe_typed_array!( u32, 4 );
impl_for_unsafe_typed_array!( i32, 5 );
impl_for_unsafe_typed_array!( f32, 6 );
impl_for_unsafe_typed_array!( f64, 7 );

#[derive(Debug)]
pub struct FunctionTag;

#[derive(Debug)]
pub struct NonFunctionTag;

impl< T: JsSerialize > JsSerializeOwned for Newtype< (NonFunctionTag, ()), T > {
    #[inline]
    fn into_js_owned< 'x >( value: &'x mut Option< Self >, arena: &'x PreallocatedArena ) -> SerializedValue< 'x > {
        JsSerialize::_into_js( value.as_ref().unwrap().as_ref(), arena )
    }

    #[inline]
    fn memory_required_owned( &self ) -> usize {
        JsSerialize::_memory_required( &**self )
    }
}

trait FuncallAdapter< F > {
    extern fn funcall_adapter( callback: *mut F, raw_arguments: *mut SerializedUntaggedArray );
    extern fn deallocator( callback: *mut F );
}

macro_rules! impl_for_fn {
    ($next:tt => $($kind:ident),*) => {
        impl< $($kind: TryFrom< Value >,)* F > FuncallAdapter< F > for Newtype< (FunctionTag, ($($kind,)*)), F >
            where F: CallMut< ($($kind,)*) > + 'static, F::Output: JsSerializeOwned
        {
            #[allow(unused_mut, unused_variables, non_snake_case)]
            extern fn funcall_adapter(
                    callback: *mut F,
                    raw_arguments: *mut SerializedUntaggedArray
                )
            {
                let callback = unsafe { &mut *callback };
                let mut arguments = unsafe { &*raw_arguments }.deserialize();

                unsafe {
                    ffi::dealloc( raw_arguments as *mut u8, mem::size_of::< SerializedValue >() );
                }

                if arguments.len() != F::expected_argument_count() {
                    // TODO: Should probably throw an exception into the JS world or something like that.
                    panic!( "Expected {} arguments, got {}", F::expected_argument_count(), arguments.len() );
                }

                let mut arguments = arguments.drain( .. );
                let mut nth_argument = 0;
                $(
                    let $kind = match arguments.next().unwrap().try_into() {
                        Ok( value ) => value,
                        Err( _ ) => {
                            panic!(
                                "Argument #{} is not convertible to '{}'",
                                nth_argument + 1,
                                type_name::< $kind >()
                            );
                        }
                    };

                    nth_argument += 1;
                )*

                $crate::private::noop( &mut nth_argument );

                let result = callback.call_mut( ($($kind,)*) );
                let mut arena = PreallocatedArena::new( result.memory_required_owned() );

                let mut result = Some( result );
                let result = JsSerializeOwned::into_js_owned( &mut result, &mut arena );
                let result = &result as *const _;

                // This is kinda hacky but I'm not sure how else to do it at the moment.
                __js_raw_asm!( "Module.STDWEB_PRIVATE.tmp = Module.STDWEB_PRIVATE.to_js( $0 );", result );
            }

            extern fn deallocator( callback: *mut F ) {
                let callback = unsafe {
                    Box::from_raw( callback )
                };

                drop( callback );
            }
        }

        impl< $($kind: TryFrom< Value >,)* F > FuncallAdapter< F > for Newtype< (FunctionTag, ($($kind,)*)), Once< F > >
            where F: CallOnce< ($($kind,)*) > + 'static, F::Output: JsSerializeOwned
        {
            #[allow(unused_mut, unused_variables, non_snake_case)]
            extern fn funcall_adapter(
                    callback: *mut F,
                    raw_arguments: *mut SerializedUntaggedArray
                )
            {
                debug_assert_ne!( callback, 0 as *mut F );

                let callback = unsafe {
                    Box::from_raw( callback )
                };

                let mut arguments = unsafe { &*raw_arguments }.deserialize();
                unsafe {
                    ffi::dealloc( raw_arguments as *mut u8, mem::size_of::< SerializedValue >() );
                }

                if arguments.len() != F::expected_argument_count() {
                    // TODO: Should probably throw an exception into the JS world or something like that.
                    panic!( "Expected {} arguments, got {}", F::expected_argument_count(), arguments.len() );
                }

                let mut arguments = arguments.drain( .. );
                let mut nth_argument = 0;
                $(
                    let $kind = match arguments.next().unwrap().try_into() {
                        Ok( value ) => value,
                        Err( _ ) => {
                            panic!(
                                "Argument #{} is not convertible to '{}'",
                                nth_argument + 1,
                                type_name::< $kind >()
                            );
                        }
                    };

                    nth_argument += 1;
                )*

                $crate::private::noop( &mut nth_argument );

                let result = callback.call_once( ($($kind,)*) );
                let mut arena = PreallocatedArena::new( result.memory_required_owned() );

                let mut result = Some( result );
                let result = JsSerializeOwned::into_js_owned( &mut result, &mut arena );
                let result = &result as *const _;

                // This is kinda hacky but I'm not sure how else to do it at the moment.
                __js_raw_asm!( "Module.STDWEB_PRIVATE.tmp = Module.STDWEB_PRIVATE.to_js( $0 );", result );
            }

            extern fn deallocator( callback: *mut F ) {
                let callback = unsafe {
                    Box::from_raw( callback )
                };

                drop( callback );
            }
        }

        impl< $($kind: TryFrom< Value >,)* F > JsSerializeOwned for Newtype< (FunctionTag, ($($kind,)*)), Once< F > >
            where F: CallOnce< ($($kind,)*) > + 'static, F::Output: JsSerializeOwned
        {
            #[inline]
            fn into_js_owned< 'a >( value: &'a mut Option< Self >, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
                let callback: *mut F = Box::into_raw( Box::new( value.take().unwrap().unwrap_newtype().0 ) );

                SerializedUntaggedFunctionOnce {
                    adapter_pointer: <Self as FuncallAdapter< F > >::funcall_adapter as u32,
                    pointer: callback as u32,
                    deallocator_pointer: <Self as FuncallAdapter< F > >::deallocator as u32
                }.into()
            }

            #[inline]
            fn memory_required_owned( &self ) -> usize {
                0
            }
        }

        impl< $($kind: TryFrom< Value >,)* F > JsSerializeOwned for Newtype< (FunctionTag, ($($kind,)*)), F >
            where F: CallMut< ($($kind,)*) > + 'static, F::Output: JsSerializeOwned
        {
            #[inline]
            fn into_js_owned< 'a >( value: &'a mut Option< Self >, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
                let callback: *mut F = Box::into_raw( Box::new( value.take().unwrap().unwrap_newtype() ) );

                SerializedUntaggedFunction {
                    adapter_pointer: <Self as FuncallAdapter< F > >::funcall_adapter as u32,
                    pointer: callback as u32,
                    deallocator_pointer: <Self as FuncallAdapter< F > >::deallocator as u32
                }.into()
            }

            #[inline]
            fn memory_required_owned( &self ) -> usize {
                0
            }
        }

        impl< $($kind: TryFrom< Value >,)* F > JsSerializeOwned for Newtype< (FunctionTag, ($($kind,)*)), Option< F > >
            where F: CallMut< ($($kind,)*) > + 'static, F::Output: JsSerializeOwned
        {
            #[inline]
            fn into_js_owned< 'a >( value: &'a mut Option< Self >, _: &'a PreallocatedArena ) -> SerializedValue< 'a > {
                if let Some( value ) = value.take().unwrap().unwrap_newtype() {
                    let callback: *mut F = Box::into_raw( Box::new( value ) );
                    SerializedUntaggedFunction {
                        adapter_pointer: <Newtype< (FunctionTag, ($($kind,)*)), F > as FuncallAdapter< F > >::funcall_adapter as u32,
                        pointer: callback as u32,
                        deallocator_pointer: <Newtype< (FunctionTag, ($($kind,)*)), F > as FuncallAdapter< F > >::deallocator as u32
                    }.into()
                } else {
                    SerializedUntaggedNull.into()
                }
            }

            #[inline]
            fn memory_required_owned( &self ) -> usize {
                0
            }
        }

        next! { $next }
    }
}

loop_through_identifiers!( impl_for_fn );

impl< 'a, T: ?Sized + JsSerialize > JsSerialize for &'a T {
    #[doc(hidden)]
    #[inline]
    fn _into_js< 'x >( &'x self, arena: &'x PreallocatedArena ) -> SerializedValue< 'x > {
        T::_into_js( *self, arena )
    }

    #[doc(hidden)]
    #[inline]
    fn _memory_required( &self ) -> usize {
        T::_memory_required( *self )
    }
}

#[cfg(test)]
mod test_deserialization {
    use std::rc::Rc;
    use std::cell::Cell;
    use super::*;

    #[test]
    fn i32() {
        assert_eq!( js! { return 100; }, Value::Number( 100_i32.into() ) );
    }

    #[test]
    fn f64() {
        assert_eq!( js! { return 100.5; }, Value::Number( 100.5_f64.into() ) );
    }

    #[test]
    fn bool_true() {
        assert_eq!( js! { return true; }, Value::Bool( true ) );
    }

    #[test]
    fn bool_false() {
        assert_eq!( js! { return false; }, Value::Bool( false ) );
    }

    #[test]
    fn undefined() {
        assert_eq!( js! { return undefined; }, Value::Undefined );
    }

    #[test]
    fn null() {
        assert_eq!( js! { return null; }, Value::Null );
    }

    #[test]
    fn string() {
        assert_eq!( js! { return "Dog"; }, Value::String( "Dog".to_string() ) );
    }

    #[test]
    fn empty_string() {
        assert_eq!( js! { return ""; }, Value::String( "".to_string() ) );
    }

    #[test]
    fn symbol() {
        let value = js! { return Symbol(); };
        assert!( value.is_symbol() );
    }

    #[test]
    fn array() {
        assert_eq!( js! { return [1, 2]; }.is_array(), true );
    }

    #[test]
    fn object() {
        assert_eq!( js! { return {"one": 1, "two": 2}; }.is_object(), true );
    }

    #[test]
    fn object_into_btreemap() {
        let object = js! { return {"one": 1, "two": 2}; }.into_object().unwrap();
        let object: BTreeMap< String, Value > = object.into();
        assert_eq!( object, [
            ("one".to_string(), Value::Number(1.into())),
            ("two".to_string(), Value::Number(2.into()))
        ].iter().cloned().collect() );
    }

    #[test]
    fn object_into_hashmap() {
        let object = js! { return {"one": 1, "two": 2}; }.into_object().unwrap();
        let object: HashMap< String, Value > = object.into();
        assert_eq!( object, [
            ("one".to_string(), Value::Number(1.into())),
            ("two".to_string(), Value::Number(2.into()))
        ].iter().cloned().collect() );
    }

    #[test]
    fn array_into_vector() {
        let array = js! { return ["one", 1]; }.into_array().unwrap();
        let array: Vec< Value > = array.into();
        assert_eq!( array, &[
            Value::String( "one".to_string() ),
            Value::Number( 1.into() )
        ]);
    }

    #[test]
    fn reference() {
        assert_eq!( js! { return new Date(); }.is_reference(), true );
    }

    #[test]
    fn arguments() {
        let value = js! {
            return (function() {
                return arguments;
            })( 1, 2 );
        };

        assert_eq!( value.is_array(), false );
    }

    #[test]
    fn function() {
        let value = Rc::new( Cell::new( 0 ) );
        let fn_value = value.clone();

        js! {
            var callback = @{move || { fn_value.set( 1 ); }};
            callback();
            callback.drop();
        };

        assert_eq!( value.get(), 1 );
    }

    #[test]
    fn function_returning_bool() {
        let result = js! {
            var callback = @{move || { return true }};
            var result = callback();
            callback.drop();

            return result;
        };

        assert_eq!( result, Value::Bool( true ) );
    }

    #[test]
    fn function_with_single_bool_argument() {
        let value = Rc::new( Cell::new( false ) );
        let fn_value = value.clone();

        js! {
            var callback = @{move |value: bool| { fn_value.set( value ); }};
            callback( true );
            callback.drop();
        };

        assert_eq!( value.get(), true );
    }

    #[test]
    fn function_inside_an_option() {
        let value = Rc::new( Cell::new( 0 ) );
        let fn_value = value.clone();

        js! {
            var callback = @{Some( move || { fn_value.set( 1 ); } )};
            callback();
            callback.drop();
        };

        assert_eq!( value.get(), 1 );
    }

    #[test]
    #[allow(unused_assignments)]
    fn function_inside_an_empty_option() {
        let mut callback = Some( move || () );
        callback = None;

        let result = js! {
            var callback = @{callback};
            return callback === null;
        };

        assert_eq!( result, Value::Bool( true ) );
    }

    #[test]
    fn function_once() {
        fn call< F: FnOnce( String ) -> String + 'static >( callback: F ) -> Value {
            js!(
                var callback = @{Once( callback )};
                return callback( "Dog" );
            )
        }

        let suffix = "!".to_owned();
        let result = call( move |value| { return value + suffix.as_str() } );
        assert_eq!( result, Value::String( "Dog!".to_owned() ) );
    }

    #[test]
    fn function_once_cannot_be_called_twice() {
        fn call< F: FnOnce() + 'static >( callback: F ) -> Value {
            js!(
                var callback = @{Once( callback )};
                callback();

                try {
                    callback();
                } catch( error ) {
                    if( error instanceof ReferenceError ) {
                        return true;
                    }
                }

                return false;
            )
        }

        let result = call( move || {} );
        assert_eq!( result, Value::Bool( true ) );
    }

    #[test]
    fn function_once_cannot_be_called_after_being_dropped() {
        fn call< F: FnOnce() + 'static >( callback: F ) -> Value {
            js!(
                var callback = @{Once( callback )};
                callback.drop();

                try {
                    callback();
                } catch( error ) {
                    if( error instanceof ReferenceError ) {
                        return true;
                    }
                }

                return false;
            )
        }

        let result = call( move || {} );
        assert_eq!( result, Value::Bool( true ) );
    }

    #[test]
    fn function_once_calling_drop_after_being_called_does_not_do_anything() {
        fn call< F: FnOnce() + 'static >( callback: F ) -> Value {
            js!(
                var callback = @{Once( callback )};
                callback();
                callback.drop();

                return true;
            )
        }

        let result = call( move || {} );
        assert_eq!( result, Value::Bool( true ) );
    }

    #[test]
    fn function_once_calling_drop_twice_does_not_do_anything() {
        fn call< F: FnOnce() + 'static >( callback: F ) -> Value {
            js!(
                var callback = @{Once( callback )};
                callback.drop();
                callback.drop();

                return true;
            )
        }

        let result = call( move || {} );
        assert_eq!( result, Value::Bool( true ) );
    }
}

#[cfg(test)]
mod test_serialization {
    use super::*;
    use std::borrow::Cow;

    #[test]
    fn object_from_btreemap() {
        let object: BTreeMap< _, _ > = [
            ("number".to_string(), Value::Number( 123.into() )),
            ("string".to_string(), Value::String( "Hello!".into() ))
        ].iter().cloned().collect();

        let result = js! {
            var object = @{object};
            return object.number === 123 && object.string === "Hello!" && Object.keys( object ).length === 2;
        };
        assert_eq!( result, Value::Bool( true ) );
    }

    #[test]
    fn object_from_borrowed_btreemap() {
        let object: BTreeMap< _, _ > = [
            ("number".to_string(), Value::Number( 123.into() ))
        ].iter().cloned().collect();

        let result = js! {
            var object = @{&object};
            return object.number === 123 && Object.keys( object ).length === 1;
        };
        assert_eq!( result, Value::Bool( true ) );
    }

    #[test]
    fn object_from_btreemap_with_convertible_key_and_value() {
        let key: Cow< str > = "number".into();
        let object: BTreeMap< _, _ > = [
            (key, 123)
        ].iter().cloned().collect();

        let result = js! {
            var object = @{object};
            return object.number === 123 && Object.keys( object ).length === 1;
        };
        assert_eq!( result, Value::Bool( true ) );
    }

    #[test]
    fn object_from_hashmap() {
        let object: HashMap< _, _ > = [
            ("number".to_string(), Value::Number( 123.into() )),
            ("string".to_string(), Value::String( "Hello!".into() ))
        ].iter().cloned().collect();

        let result = js! {
            var object = @{object};
            return object.number === 123 && object.string === "Hello!" && Object.keys( object ).length === 2;
        };
        assert_eq!( result, Value::Bool( true ) );
    }

    #[test]
    fn vector_of_strings() {
        let vec: Vec< _ > = vec![
            "one".to_string(),
            "two".to_string()
        ];

        let result = js! {
            var vec = @{vec};
            return vec[0] === "one" && vec[1] === "two" && vec.length === 2;
        };
        assert_eq!( result, Value::Bool( true ) );
    }

    #[test]
    fn multiple() {
        let reference: Reference = js! {
            return new Date();
        }.try_into().unwrap();

        let result = js! {
            var callback = @{|| {}};
            var reference = @{&reference};
            var string = @{"Hello!"};
            return Object.prototype.toString.call( callback ) === "[object Function]" &&
                Object.prototype.toString.call( reference ) === "[object Date]" &&
                Object.prototype.toString.call( string ) === "[object String]"
        };
        assert_eq!( result, Value::Bool( true ) );
    }

    #[test]
    fn serialize_0() {
        assert_eq!(
            js! { return 0; },
            0
        );
    }

    #[test]
    fn serialize_1() {
        assert_eq!(
            js! { return @{1}; },
            1
        );
    }

    #[test]
    fn serialize_2() {
        assert_eq!(
            js! { return @{1} + @{2}; },
            1 + 2
        );
    }

    #[test]
    fn serialize_3() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3}; },
            1 + 2 + 3
        );
    }

    #[test]
    fn serialize_4() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4}; },
            1 + 2 + 3 + 4
        );
    }

    #[test]
    fn serialize_5() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4} + @{5}; },
            1 + 2 + 3 + 4 + 5
        );
    }

    #[test]
    fn serialize_6() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4} + @{5} + @{6}; },
            1 + 2 + 3 + 4 + 5 + 6
        );
    }

    #[test]
    fn serialize_7() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4} + @{5} + @{6} + @{7}; },
            1 + 2 + 3 + 4 + 5 + 6 + 7
        );
    }

    #[test]
    fn serialize_8() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4} + @{5} + @{6} + @{7} + @{8}; },
            1 + 2 + 3 + 4 + 5 + 6 + 7 + 8
        );
    }

    #[test]
    fn serialize_9() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4} + @{5} + @{6} + @{7} + @{8} + @{9}; },
            1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9
        );
    }

    #[test]
    fn serialize_10() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4} + @{5} + @{6} + @{7} + @{8} + @{9} + @{10}; },
            1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10
        );
    }

    #[test]
    fn serialize_11() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4} + @{5} + @{6} + @{7} + @{8} + @{9} + @{10} + @{11}; },
            1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 + 11
        );
    }

    #[test]
    fn serialize_12() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4} + @{5} + @{6} + @{7} + @{8} + @{9} + @{10} + @{11} + @{12}; },
            1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 + 11 + 12
        );
    }

    #[test]
    fn serialize_13() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4} + @{5} + @{6} + @{7} + @{8} + @{9} + @{10} + @{11} + @{12} + @{13}; },
            1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 + 11 + 12 + 13
        );
    }

    #[test]
    fn serialize_14() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4} + @{5} + @{6} + @{7} + @{8} + @{9} + @{10} + @{11} + @{12} + @{13} + @{14}; },
            1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 + 11 + 12 + 13 + 14
        );
    }

    #[test]
    fn serialize_15() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4} + @{5} + @{6} + @{7} + @{8} + @{9} + @{10} + @{11} + @{12} + @{13} + @{14} + @{15}; },
            1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 + 11 + 12 + 13 + 14 + 15
        );
    }

    #[test]
    fn serialize_16() {
        assert_eq!(
            js! { return @{1} + @{2} + @{3} + @{4} + @{5} + @{6} + @{7} + @{8} + @{9} + @{10} + @{11} + @{12} + @{13} + @{14} + @{15} + @{16}; },
            1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 + 11 + 12 + 13 + 14 + 15 + 16
        );
    }

    #[test]
    fn interpolated_args_are_converted_at_the_start() {
        let mut string = "1".to_owned();
        let callback = js! {
            return function() {
                return @{&string};
            }
        };

        unsafe {
            string.as_bytes_mut()[0] = b'2';
        }

        let result = js! {
            return @{callback}();
        };

        assert_eq!( result, "1" );
    }

    macro_rules! test_unsafe_typed_array {
        ($test_name:ident, $ty:ty, $js_type_name:ident) => {
            #[allow(trivial_numeric_casts)]
            #[test]
            fn $test_name() {
                let slice: &[$ty] = &[1 as $ty, 2 as $ty, 3 as $ty];
                let slice = unsafe { UnsafeTypedArray::new( slice ) };
                let result: Vec< Value > = js!(
                    var slice = @{slice};
                    var sum = slice[0] + slice[1] + slice[2];
                    var name = slice.constructor.name;
                    var length = slice.length;
                    return [name, sum, length]
                ).try_into().unwrap();

                let mut result = result.into_iter();
                let name: String = result.next().unwrap().try_into().unwrap();
                let sum: f64 = result.next().unwrap().try_into().unwrap();
                let length: usize = result.next().unwrap().try_into().unwrap();

                assert_eq!( name, stringify!( $js_type_name ) );
                assert_eq!( sum as u64, 6 );
                assert_eq!( length, 3 );
            }
        }
    }

    test_unsafe_typed_array!( test_unsafe_typed_array_u8, u8, Uint8Array );
    test_unsafe_typed_array!( test_unsafe_typed_array_i8, i8, Int8Array );
    test_unsafe_typed_array!( test_unsafe_typed_array_u16, u16, Uint16Array );
    test_unsafe_typed_array!( test_unsafe_typed_array_i16, i16, Int16Array );
    test_unsafe_typed_array!( test_unsafe_typed_array_u32, u32, Uint32Array );
    test_unsafe_typed_array!( test_unsafe_typed_array_i32, i32, Int32Array );
    test_unsafe_typed_array!( test_unsafe_typed_array_f32, f32, Float32Array );
    test_unsafe_typed_array!( test_unsafe_typed_array_f64, f64, Float64Array );
}

#[cfg(test)]
mod test_reserialization {
    use super::*;
    use webcore::array::Array;

    #[test]
    fn i32() {
        assert_eq!( js! { return @{100}; }, Value::Number( 100_i32.into() ) );
    }

    #[test]
    fn f64() {
        assert_eq!( js! { return @{100.5}; }, Value::Number( 100.5_f64.into() ) );
    }

    #[test]
    fn bool_true() {
        assert_eq!( js! { return @{true}; }, Value::Bool( true ) );
    }

    #[test]
    fn bool_false() {
        assert_eq!( js! { return @{false}; }, Value::Bool( false ) );
    }

    #[test]
    fn undefined() {
        assert_eq!( js! { return @{Undefined}; }, Value::Undefined );
    }

    #[test]
    fn null() {
        assert_eq!( js! { return @{Null}; }, Value::Null );
    }

    #[test]
    fn string() {
        assert_eq!( js! { return @{"Dog"}; }, Value::String( "Dog".to_string() ) );
    }

    #[test]
    fn empty_string() {
        assert_eq!( js! { return @{""}; }, Value::String( "".to_string() ) );
    }

    #[test]
    fn string_with_non_bmp_character() {
        assert_eq!( js! { return @{"😐"} + ", 😐"; }, Value::String( "😐, 😐".to_string() ) );
    }

    #[test]
    fn array() {
        let array: Array = vec![ Value::Number( 1.into() ), Value::Number( 2.into() ) ].into();
        assert_eq!( js! { return @{&array}; }.into_reference().unwrap(), *array.as_ref() );
    }

    #[test]
    fn array_values_are_not_compared_by_value() {
        let array: Array = vec![ Value::Number( 1.into() ), Value::Number( 2.into() ) ].into();
        assert_ne!( js! { return @{&[1, 2][..]}; }.into_reference().unwrap(), *array.as_ref() );
    }

    #[test]
    fn object() {
        let object: BTreeMap< _, _ > = [
            ("one".to_string(), Value::Number( 1.into() )),
            ("two".to_string(), Value::Number( 2.into() ))
        ].iter().cloned().collect();

        let object: Value = object.into();
        assert_eq!( js! { return @{&object} }, object );
    }

    #[test]
    fn symbol() {
        let value_1: Symbol = js! { return Symbol(); }.try_into().unwrap();
        let value_2: Symbol = js! { return @{&value_1}; }.try_into().unwrap();
        assert_eq!( value_1, value_2 );
        assert_eq!( js! { return @{value_1} === @{value_2}; }, true );
    }

    #[test]
    fn cloned_symbol() {
        let value_1: Symbol = js! { return Symbol(); }.try_into().unwrap();
        let value_2 = value_1.clone();
        assert_eq!( value_1, value_2 );
        assert_eq!( js! { return @{value_1} === @{value_2}; }, true );
    }

    #[test]
    fn different_symbols() {
        let value_1: Symbol = js! { return Symbol(); }.try_into().unwrap();
        let value_2: Symbol = js! { return Symbol(); }.try_into().unwrap();
        assert_ne!( value_1, value_2 );
        assert_eq!( js! { return @{value_1} !== @{value_2}; }, true );
    }

    #[test]
    fn reference() {
        let date = js! { return new Date(); };
        assert_eq!( js! { return Object.prototype.toString.call( @{date} ) }, "[object Date]" );
    }

    #[test]
    fn reference_by_ref() {
        let date = js! { return new Date(); };
        assert_eq!( js! { return Object.prototype.toString.call( @{&date} ) }, "[object Date]" );
    }

    #[test]
    fn option_some() {
        assert_eq!( js! { return @{Some( true )}; }, Value::Bool( true ) );
    }

    #[test]
    fn option_none() {
        let boolean_none: Option< bool > = None;
        assert_eq!( js! { return @{boolean_none}; }, Value::Null );
    }

    #[test]
    fn value() {
        assert_eq!( js! { return @{Value::String( "Dog".to_string() )}; }, Value::String( "Dog".to_string() ) );
    }

    #[test]
    fn closure_context() {
        let constant: u32 = 0x12345678;
        let callback = move || {
            let value: Value = constant.into();
            value
        };

        let value = js! {
            return @{callback}();
        };

        assert_eq!( value, Value::Number( 0x12345678_i32.into() ) );
    }

    #[test]
    fn string_identity_function() {
        fn identity( string: String ) -> String {
            string
        }

        let empty = js! {
            var identity = @{identity};
            return identity( "" );
        };

        assert_eq!( empty, Value::String( "".to_string() ) );

        let non_empty = js! {
            var identity = @{identity};
            return identity( "死神はりんごしか食べない!" );
        };

        assert_eq!( non_empty, Value::String( "死神はりんごしか食べない!".to_string() ) );
    }

    #[derive(Clone, Debug, PartialEq, Eq, ReferenceType)]
    #[reference(instance_of = "Error")]
    pub struct Error( Reference );

    #[test]
    fn closure_returning_reference_object() {
        fn identity( error: Error ) -> Error {
            error
        }

        let value = js! {
            var identity = @{identity};
            return identity( new ReferenceError() );
        };

        assert!( instanceof!( value, Error ) );
    }
}
