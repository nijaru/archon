use std::fmt;

macro_rules! id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
        pub struct $name(u64);

        impl $name {
            pub const fn from_u64(value: u64) -> Self {
                Self(value)
            }

            pub const fn as_u64(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }
    };
}

id!(NodeId);
id!(LeaseId);
id!(BindingId);
id!(RequestId);
id!(OwnerId);
id!(ProviderId);

impl ProviderId {
    pub const ENFORCE: Self = Self(1);
}
