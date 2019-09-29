/*
@author: xiao cai niao
@datetime: 2019/9/25
*/
use crate::{readvalue, Config};
use crate::meta;
use std::{process, io};
use crate::readvalue::read_string_value;
use std::borrow::Borrow;
use uuid;
use uuid::Uuid;
use std::io::{Read, Cursor, Seek, SeekFrom, Result};
use crate::meta::ColumnTypeDict;
use byteorder::{ReadBytesExt, LittleEndian};
use failure::_core::str::from_utf8;


pub trait Tell: Seek {
    fn tell(&mut self) -> Result<u64> {
        self.seek(SeekFrom::Current(0))
    }
}

impl<T> Tell for T where T: Seek { }


#[derive(Debug)]
pub enum BinlogEvent{
    QueryEvent,
    RotateLogEvent,
    TableMapEvent,
    GtidEvent,
    UpdateEvent,
    WriteEvent,
    DeleteEvent,
    XidEvent,
    XAPREPARELOGEVENT,
    UNKNOWNEVENT
}

pub trait InitHeader{
    fn new<R: Read+Seek>(buf: &mut R, conf: &Config) -> Self;
}

pub trait InitValue{
    fn read_event<R: Read+Seek>(header: &EventHeader, buf: &mut R) -> Self;
}


/*
binlog包头部分
    binlog_event_header_len = 19
    timestamp : 4bytes
    type_code : 1bytes
    server_id : 4bytes
    event_length : 4bytes
    next_position : 4bytes
    flags : 2bytes
*/
#[derive(Debug)]
pub struct EventHeader{
    //19bytes 包头部分
    pub timestamp: u32,
    pub type_code: BinlogEvent,
    pub server_id: u32,
    pub event_length: u32,
    pub next_position: u32,
    pub flags: u16,
    pub header_length: u8,
}

impl InitHeader for EventHeader {
    fn new<R: Read + Seek>(buf: &mut R, conf: &Config) -> EventHeader{
        let mut header_length: u8 = 19;
        if conf.conntype == String::from("repl"){
            //如果是模拟slave同步会多亿字节的头部分
            buf.seek(io::SeekFrom::Current(1));
            header_length += 1;
        }
        let timestamp = buf.read_u32::<LittleEndian>().unwrap();
        let type_code = Self::get_type_code_event(&Some(buf.read_u8().unwrap() as u8));
        let server_id = buf.read_u32::<LittleEndian>().unwrap();
        let event_length = buf.read_u32::<LittleEndian>().unwrap();
        let next_position = buf.read_u32::<LittleEndian>().unwrap();
        let flags = buf.read_u16::<LittleEndian>().unwrap();
        EventHeader{
            timestamp,
            type_code,
            server_id,
            event_length,
            next_position,
            flags,
            header_length
        }
    }
}

impl EventHeader{
    fn get_type_code_event(type_code: &Option<u8>) -> BinlogEvent{
        match type_code {
            Some(4) => BinlogEvent::RotateLogEvent,
            Some(2) => BinlogEvent::QueryEvent,
            Some(33) => BinlogEvent::GtidEvent,
            Some(19) => BinlogEvent::TableMapEvent,
            Some(30) => BinlogEvent::WriteEvent,
            Some(31) => BinlogEvent::UpdateEvent,
            Some(32) => BinlogEvent::DeleteEvent,
            Some(33) => BinlogEvent::GtidEvent,
            Some(16) => BinlogEvent::XidEvent,
            Some(38) => BinlogEvent::XAPREPARELOGEVENT,
            _ => BinlogEvent::UNKNOWNEVENT
        }
    }
}

/*
query_event:
    fix_part = 13:
        thread_id : 4bytes
        execute_seconds : 4bytes
        database_length : 1bytes
        error_code : 2bytes
        variable_block_length : 2bytes
    variable_part :
        variable_block_length = fix_part.variable_block_length
        database_name = fix_part.database_length
        sql_statement = event_header.event_length - 19 - 13 - variable_block_length - database_length - 4
*/
#[derive(Debug)]
pub struct QueryEvent{
    pub thread_id: u32,
    pub execute_seconds: u32,
    pub database: String,
    pub command: String
}

impl InitValue for QueryEvent{
    fn read_event<R: Read+Seek>(header: &EventHeader, buf: &mut R) -> QueryEvent{
        let thread_id = buf.read_u32::<LittleEndian>().unwrap();
        let execute_seconds = buf.read_u32::<LittleEndian>().unwrap();
        let database_length = buf.read_u8().unwrap();
        let error_code = buf.read_u16::<LittleEndian>().unwrap();
        let variable_block_length = buf.read_u16::<LittleEndian>().unwrap();
        buf.seek(io::SeekFrom::Current(variable_block_length as i64));
        let mut database_pack = vec![0u8; database_length as usize];
        buf.read_exact(&mut database_pack);
        let database = readvalue::read_string_value(&database_pack);
        buf.seek(io::SeekFrom::Current(1));

        let command_length = header.event_length as usize - buf.tell().unwrap() as usize;
        let mut command_pak = vec![];
        buf.read_to_end(&mut command_pak);
        let command = readvalue::read_string_value(&command_pak);

        QueryEvent{
            thread_id,
            execute_seconds,
            database,
            command
        }

    }
}

#[derive(Debug)]
pub struct XidEvent{
    pub xid: u64
}

impl InitValue for XidEvent{
    fn read_event<R: Read>(header: &EventHeader, buf: &mut R) -> XidEvent{
        let xid = buf.read_u64::<LittleEndian>().unwrap();
        XidEvent{
            xid
        }
    }
}

/*
rotate_log_event:
    Fixed data part: 8bytes
    Variable data part: event_length - header_length - fixed_length (string<EOF>)
*/
#[derive(Debug)]
pub struct RotateLog{
    pub binlog_file: String
}

impl InitValue for RotateLog{
    fn read_event<R: Read+Seek>(header: &EventHeader, buf: &mut R) -> RotateLog{
        let fixed_length: usize = 8;
        buf.seek(io::SeekFrom::Current(8));
        let num= header.event_length as usize - header.header_length as usize - fixed_length;
        let binlog_file = readvalue::read_string_value_from_len(buf, num);
        RotateLog{
            binlog_file
        }
    }
}

/*
table_map_event:
    fix_part = 8
        table_id : 6bytes
        Reserved : 2bytes
    variable_part:
        database_name_length : 1bytes
        database_name : database_name_length bytes + 1
        table_name_length : 1bytes
        table_name : table_name_length bytes + 1
        cloums_count : 1bytes
        colums_type_array : one byte per column
        mmetadata_lenth : 1bytes
        metadata : .....(only available in the variable length field，varchar:2bytes，text、blob:1bytes,time、timestamp、datetime: 1bytes
                        blob、float、decimal : 1bytes, char、enum、binary、set: 2bytes(column type id :1bytes metadatea: 1bytes))
        bit_filed : 1bytes
        crc : 4bytes
        .........
*/
#[derive(Debug)]
pub struct ColumnInfo {
    pub column_type: ColumnTypeDict,
    pub column_meta: Vec<usize>
}

#[derive(Debug)]
pub struct TableMap{
    pub database_name: String,
    pub table_name: String,
    pub column_count: u8,
    pub column_info: Vec<ColumnInfo>,
}
impl TableMap{
    pub fn new() -> TableMap {
        TableMap{
            database_name: "".to_string(),
            table_name: "".to_string(),
            column_count: 0,
            column_info: vec![]
        }
    }

    fn read_column_meta<R: Read>(buf: &mut R,col_type: &u8) -> Vec<usize> {
        let mut value: Vec<usize> = vec![];
        //let mut offset = offset;
        let column_type_info = ColumnTypeDict::from_type_code(col_type);
        match column_type_info {
            ColumnTypeDict::MYSQL_TYPE_VAR_STRING => {
                value = Self::read_string_meta(buf);
            }
            ColumnTypeDict::MYSQL_TYPE_VARCHAR => {
                value = Self::read_string_meta(buf);
            }
            ColumnTypeDict::MYSQL_TYPE_BLOB => {
                value = Self::read_one_bytes(buf);
            }
            ColumnTypeDict::MYSQL_TYPE_MEDIUM_BLOB => {
                value = Self::read_one_bytes(buf);
            }
            ColumnTypeDict::MYSQL_TYPE_LONG_BLOB => {
                value = Self::read_one_bytes(buf);
            }
            ColumnTypeDict::MYSQL_TYPE_TINY_BLOB => {
                value = Self::read_one_bytes(buf);
            }
            ColumnTypeDict::MYSQL_TYPE_JSON => {
                value = Self::read_one_bytes(buf);
            }
            ColumnTypeDict::MYSQL_TYPE_TIMESTAMP2 => {
                value = Self::read_one_bytes(buf);
            }
            ColumnTypeDict::MYSQL_TYPE_DATETIME2 => {
                value = Self::read_one_bytes(buf);
            }
            ColumnTypeDict::MYSQL_TYPE_TIME2 => {
                //value = vec![buf[offset] as usize];
                //offset += 1;
                value = Self::read_one_bytes(buf);
            }
            ColumnTypeDict::MYSQL_TYPE_NEWDECIMAL => {
                value.extend(Self::read_newdecimal(buf).to_owned().to_vec());
            }
            ColumnTypeDict::MYSQL_TYPE_FLOAT => {
                value = Self::read_one_bytes(buf);
            }
            ColumnTypeDict::MYSQL_TYPE_DOUBLE => {
                value = Self::read_one_bytes(buf);
            }
            ColumnTypeDict::MYSQL_TYPE_STRING => {
                value = Self::read_string_type(buf, col_type);
            }
            _ => {
                value = vec![0];
            }
        }
        return value;
    }

    fn read_one_bytes<R: Read>(buf: &mut R) -> Vec<usize> {
        let v = buf.read_u8().unwrap() as usize;
        vec![v]
    }


    fn read_string_meta<R: Read>(buf: &mut R) -> Vec<usize> {
        let metadata = buf.read_u16::<LittleEndian>().unwrap();
        let mut v = vec![];
        if metadata > 255 {
            v.push(2);
        }else {
            v.push(1);
        }
        v
    }

    fn read_newdecimal<R: Read>(buf: &mut R) -> [usize;2] {
        let precision = buf.read_u8().unwrap() as usize;
        let decimals = buf.read_u8().unwrap() as usize;
        [precision,decimals]
    }

    fn read_string_type<R: Read>(buf: &mut R,col_type: &u8) -> Vec<usize> {
        let _type = buf.read_u8().unwrap();
        let metadata = buf.read_u8().unwrap() as usize;
        if col_type != &_type{
            vec![65535]
        }else {
            vec! [metadata]
        }
    }

}

impl InitValue for TableMap{
    fn read_event<R: Read+Seek>( header: &EventHeader,buf: &mut R) -> TableMap{
        buf.seek(io::SeekFrom::Current(8));
        let database_length = buf.read_u8().unwrap() as usize;
        let database_name = readvalue::read_string_value_from_len(buf, database_length);
        buf.seek(io::SeekFrom::Current(1));
        let table_length = buf.read_u8().unwrap() as usize;
        let table_name = readvalue::read_string_value_from_len(buf, table_length);
        buf.seek(io::SeekFrom::Current(1));

        let column_count = buf.read_u8().unwrap();
        let mut column_info: Vec<ColumnInfo> = vec![];
        let mut column_type_list = vec![0u8; column_count as usize];
        buf.read_exact(&mut column_type_list);
        buf.seek(io::SeekFrom::Current(1)); //跳过mmetadata_lenth,直接用字段数据进行判断
        for col_type in column_type_list.iter() {
            let col_meta = Self::read_column_meta(buf, col_type);
            column_info.push(ColumnInfo{column_type: ColumnTypeDict::from_type_code(col_type),column_meta: col_meta});
        }


        TableMap{
            database_name,
            table_name,
            column_count,
            column_info
        }
    }
}

/*
gtid_event:
    The layout of the buffer is as follows:
    +------+--------+-------+-------+--------------+---------------+
    |flags |SID     |GNO    |lt_type|last_committed|sequence_number|
    |1 byte|16 bytes|8 bytes|1 byte |8 bytes       |8 bytes        |
    +------+--------+-------+-------+--------------+---------------+

    The 'flags' field contains gtid flags.
        0 : rbr_only ,"/*!50718 SET TRANSACTION ISOLATION LEVEL READ COMMITTED*/%s\n"
        1 : sbr

    lt_type (for logical timestamp typecode) is always equal to the
    constant LOGICAL_TIMESTAMP_TYPECODE.

    5.6 did not have TS_TYPE and the following fields. 5.7.4 and
    earlier had a different value for TS_TYPE and a shorter length for
    the following fields. Both these cases are accepted and ignored.

    The buffer is advanced in Binary_log_event constructor to point to
    beginning of post-header
*/

#[derive(Debug)]
pub struct GtidEvent{
    pub gtid: Uuid,
    pub gno_id: u64,
    pub last_committed: u64,
    pub sequence_number: u64
}

impl InitValue for GtidEvent {
    fn read_event<R: Read+Seek>(header: &EventHeader, buf: &mut R) -> GtidEvent {
        buf.seek(io::SeekFrom::Current(1));
        let mut sid = [0 as u8; 16];
        buf.read_exact(&mut sid);

        let gtid = uuid::Uuid::from_bytes(sid);
        let gno_id = buf.read_u64::<LittleEndian>().unwrap();

        let last_committed = buf.read_u64::<LittleEndian>().unwrap();
        let sequence_number = buf.read_u64::<LittleEndian>().unwrap();

        GtidEvent{
            gtid,
            gno_id,
            last_committed,
            sequence_number
        }
    }
}
