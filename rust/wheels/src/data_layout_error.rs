#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DataLayoutError {
    InvalidDataLength = DataLayoutError::MIN + 0,
    InvalidBufferOffset = DataLayoutError::MIN + 1,
    InvalidOptionTag = DataLayoutError::MIN + 2,
    LengthExceedsCapacity = DataLayoutError::MIN + 3,
    OutputBufferTooSmall = DataLayoutError::MIN + 4,
    MissingOptionTag = DataLayoutError::MIN + 5,
    TruncatedPayload = DataLayoutError::MIN + 6,
    InvalidFieldAlignment = DataLayoutError::MIN + 7,
    MissingLengthHeader = DataLayoutError::MIN + 8,
    TruncatedVectorPayload = DataLayoutError::MIN + 9,
    InvalidImplicitOptionEncoding = DataLayoutError::MIN + 10,
}

impl DataLayoutError {
    ///
    /// 0x444C stands for DL (DataLayout).
    ///
    /// >> println!("{:X}", u32::from_be_bytes(*b"DL\0\0"));
    ///
    /// 0x444C0000
    ///
    pub const MIN: u32 = 0x444C0000;
    pub const MAX: u32 = 0x444C00FF;

    pub const fn code(self) -> u32 {
        self as u32
    }

    pub const fn into_program_error(self) -> pinocchio::error::ProgramError {
        pinocchio::error::ProgramError::Custom(self.code())
    }
}

impl From<DataLayoutError> for pinocchio::error::ProgramError {
    fn from(value: DataLayoutError) -> Self {
        value.into_program_error()
    }
}

impl core::convert::TryFrom<u32> for DataLayoutError {
    type Error = ();

    fn try_from(value: u32) -> core::result::Result<Self, Self::Error> {
        match value {
            x if x == Self::InvalidDataLength as u32 => Ok(Self::InvalidDataLength),
            x if x == Self::InvalidBufferOffset as u32 => Ok(Self::InvalidBufferOffset),
            x if x == Self::InvalidOptionTag as u32 => Ok(Self::InvalidOptionTag),
            x if x == Self::LengthExceedsCapacity as u32 => Ok(Self::LengthExceedsCapacity),
            x if x == Self::OutputBufferTooSmall as u32 => Ok(Self::OutputBufferTooSmall),
            x if x == Self::MissingOptionTag as u32 => Ok(Self::MissingOptionTag),
            x if x == Self::TruncatedPayload as u32 => Ok(Self::TruncatedPayload),
            x if x == Self::InvalidFieldAlignment as u32 => Ok(Self::InvalidFieldAlignment),
            x if x == Self::MissingLengthHeader as u32 => Ok(Self::MissingLengthHeader),
            x if x == Self::TruncatedVectorPayload as u32 => Ok(Self::TruncatedVectorPayload),
            x if x == Self::InvalidImplicitOptionEncoding as u32 => {
                Ok(Self::InvalidImplicitOptionEncoding)
            }
            _ => Err(()),
        }
    }
}

impl core::convert::TryFrom<pinocchio::error::ProgramError> for DataLayoutError {
    type Error = pinocchio::error::ProgramError;

    fn try_from(value: pinocchio::error::ProgramError) -> core::result::Result<Self, Self::Error> {
        match value {
            pinocchio::error::ProgramError::Custom(code) => {
                Self::try_from(code).map_err(|_| pinocchio::error::ProgramError::Custom(code))
            }
            other => Err(other),
        }
    }
}

impl pinocchio::error::ToStr for DataLayoutError {
    fn to_str(&self) -> &'static str {
        match self {
            Self::InvalidDataLength => "Invalid data length",
            Self::InvalidBufferOffset => "Invalid buffer offset",
            Self::InvalidOptionTag => "Invalid option tag",
            Self::LengthExceedsCapacity => "Length exceeds declared capacity",
            Self::OutputBufferTooSmall => "Output buffer too small",
            Self::MissingOptionTag => "Missing option tag",
            Self::TruncatedPayload => "Truncated payload",
            Self::InvalidFieldAlignment => "Invalid field alignment",
            Self::MissingLengthHeader => "Missing length header",
            Self::TruncatedVectorPayload => "Truncated vector payload",
            Self::InvalidImplicitOptionEncoding => "Invalid implicit option encoding",
        }
    }
}

impl core::fmt::Display for DataLayoutError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(<Self as pinocchio::error::ToStr>::to_str(self))
    }
}

impl core::error::Error for DataLayoutError {}
